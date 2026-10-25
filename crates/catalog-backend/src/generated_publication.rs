//! Receipt-derived generated occurrence reconstruction. Stored command/receipt
//! data remains descriptive until compared with the current retained native
//! generated owner and exact private output source.
use crate::{occurrence_bridge::OccurrenceMapping, sources::SourceMapping};
use anyhow::{Context, ensure};
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_store::catalog_generated_output::GeneratedOutputRequest;
use serde_json::{Value, json};

/// Immutable original ancestry of a generated output. This is not a current
/// availability or scope grant: current source ownership/lifecycle and current
/// output ownership must be checked separately under the admission writer.
pub struct GeneratedInputSource {
    pub occurrence: OccurrenceMapping,
    pub admission_digest: String,
    pub source_mapping_digest: String,
    pub occurrence_mapping_digest: String,
}

pub fn input_source(
    command: &CommandEnvelope,
    admission: &Value,
) -> anyhow::Result<GeneratedInputSource> {
    ensure!(
        command.payload()["operation"] == "generated_output_admit",
        "generated command required"
    );
    let request: GeneratedOutputRequest =
        serde_json::from_value(command.payload()["request"].clone())?;
    let admission_digest = catabolic_core::sha256(serde_json::to_vec(admission)?);
    ensure!(
        request.preparation_digest == admission_digest && admission["job_id"] == request.job_id,
        "generated input admission changed"
    );
    let occurrence: OccurrenceMapping = serde_json::from_value(admission["occurrence"].clone())?;
    ensure!(
        occurrence.source.library_id != crate::generated_source::SOURCE_ID
            && (occurrence.motion_file["generated"].is_null()
                || occurrence.motion_file["generated"] == 0)
            && occurrence.source.catalog.catalog_id == command.key().catalog_id()
            && occurrence.source.catalog.incarnation == command.admission_incarnation()
            && occurrence.source.profile == request.profile
            && occurrence.physical.profile == request.profile
            && occurrence.physical.location == occurrence.source.native_source
            && occurrence.physical.exact == request.input
            && occurrence.motion_file["id"] == request.provenance["job"]["source_file_id"]
            && occurrence.motion_file["revision"] == request.provenance["job"]["source_revision"],
        "generated original ancestry changed"
    );
    let stamp = occurrence.motion_file["fingerprint"]
        .as_str()
        .context("generated input fingerprint missing")?;
    let parts: Vec<_> = stamp.split(':').collect();
    ensure!(
        parts.len() == 6,
        "generated input fingerprint is incomplete"
    );
    let seconds: i64 = parts[4].parse()?;
    let nanos: i64 = parts[5].parse()?;
    ensure!(
        (0..1_000_000_000).contains(&nanos),
        "invalid generated input ctime"
    );
    let ctime = seconds
        .checked_mul(1_000_000_000)
        .and_then(|n| n.checked_add(nanos))
        .context("generated input ctime overflow")?;
    ensure!(
        ctime == request.input_ctime_ns,
        "generated input ctime changed"
    );
    Ok(GeneratedInputSource {
        source_mapping_digest: catabolic_core::sha256(serde_json::to_vec(&serde_json::to_value(
            &occurrence.source,
        )?)?),
        occurrence_mapping_digest: catabolic_core::sha256(serde_json::to_vec(
            &serde_json::to_value(&occurrence)?,
        )?),
        occurrence,
        admission_digest,
    })
}

pub fn decode_command(saved: &Value) -> anyhow::Result<CommandEnvelope> {
    let command = CommandEnvelope::new(
        1,
        serde_json::from_value(saved["key"].clone())?,
        saved["admission_incarnation"]
            .as_str()
            .context("generated incarnation missing")?,
        saved["expected_generation"]
            .as_i64()
            .context("generated generation missing")?,
        &saved["payload"].to_string(),
    )?;
    ensure!(
        serde_json::from_str::<Value>(command.canonical_payload())? == *saved,
        "generated canonical command changed"
    );
    Ok(command)
}

pub fn mapping(
    command: &CommandEnvelope,
    result: &Value,
    source: &SourceMapping,
    probe: &Value,
    generation: i64,
) -> anyhow::Result<OccurrenceMapping> {
    ensure!(
        command.payload()["operation"] == "generated_output_admit",
        "generated command required"
    );
    let request: GeneratedOutputRequest =
        serde_json::from_value(command.payload()["request"].clone())?;
    crate::generated_source::validate(source)?;
    ensure!(
        command.key().catalog_id() == source.catalog.catalog_id
            && command.admission_incarnation() == source.catalog.incarnation
            && request.profile == source.profile
            && request.output.location == source.native_source
            && request.output_binding == source.native_binding
            && result["operation"] == "generated_output_admit"
            && result["job_id"] == request.job_id
            && result["attempt"] == request.attempt
            && result["output"] == serde_json::to_value(&request.output)?
            && result["preparation_digest"] == request.preparation_digest
            && result["output_sha256"] == request.output_sha256
            && result["output_ctime_ns"] == request.output_ctime_ns
            && result["output_size"] == request.output_size
            && result["output_duration_us"] == request.output_duration_us
            && result["provenance"] == request.provenance
            && generation > 0,
        "generated receipt/source proof mismatch"
    );
    ensure!(
        probe["revision"] == request.output_sha256
            && probe["bytes"] == request.output_size
            && probe["relative_path"] == request.output.path
            && probe["tracks"].is_array()
            && probe["duration_seconds"]
                .as_f64()
                .is_some_and(|value| value.is_finite()
                    && value * 1_000_000.0 == request.output_duration_us as f64),
        "generated retained probe mismatch"
    );
    let item_id = request.item["id"]
        .as_str()
        .context("generated item ID missing")?;
    let version_id = result["version_id"]
        .as_str()
        .context("generated receipt version missing")?;
    let timeline_id = result["timeline_id"]
        .as_str()
        .context("generated receipt timeline missing")?;
    let edition_native = result["edition_item_id"]
        .as_str()
        .context("generated receipt edition missing")?;
    let edition = edition_native
        .strip_prefix("motion.edition:")
        .filter(|v| !v.is_empty())
        .context("generated edition is not a Motion identity")?;
    ensure!(
        item_id == format!("motion.version:{version_id}")
            && request.item["metadata"]["motion_version"]["source_version"]["id"] == version_id
            && request.item["metadata"]["motion_version"]["source_version"]["timeline_id"]
                == timeline_id
            && request.native.associations.len() == 1
            && request.native.associations[0].item_id == item_id,
        "generated structural receipt changed"
    );
    let file_id = &request.output.source_occurrence_id;
    Ok(OccurrenceMapping {
        source: source.clone(),
        native_generation: generation,
        physical: request.output.clone(),
        motion_file: json!({"id":file_id,"edition_id":edition,"library_id":source.library_id,"relative_path":request.output.path,
            "revision":request.output_sha256,"fingerprint":probe["fingerprint"],"bytes":request.output_size,
            "duration_seconds":probe["duration_seconds"],"tracks_json":probe["tracks"].to_string(),"available":1,"generated":1,
            "chapters_json":if probe["chapters"].is_null(){Value::Null}else{Value::String(probe["chapters"].to_string())}}),
        version_files: vec![
            json!({"version_id":version_id,"part":1,"file_id":file_id,"file_revision":request.output_sha256,"start_ms":null,"end_ms":null}),
        ],
        association: request.native.associations[0].clone(),
    })
}

pub(crate) fn verify_native_item(
    command: &CommandEnvelope,
    result: &Value,
    item: &Value,
) -> anyhow::Result<()> {
    let r: GeneratedOutputRequest = serde_json::from_value(command.payload()["request"].clone())?;
    let id = r.item["id"]
        .as_str()
        .context("generated native item ID missing")?;
    let proof = json!({"job_id":r.job_id,"attempt":r.attempt,"preparation_digest":r.preparation_digest,"provenance":r.provenance,
        "input_version_id":r.input_version_id,"input_version_revision":r.input_version_revision,"input_timeline_revision":r.input_timeline_revision,
        "input":r.input,"input_ctime_ns":r.input_ctime_ns,"output":r.output.exact,"output_ctime_ns":r.output_ctime_ns,
        "output_size":r.output_size,"output_sha256":r.output_sha256,"output_duration_us":r.output_duration_us});
    ensure!(
        item["item_id"] == id
            && item["retirement"].is_null()
            && item["kind"] == "custom:motion_version"
            && item["metadata"]["motion_generated_output"] == proof
            && item["metadata"]["motion_version"] == r.item["metadata"]["motion_version"]
            && item["metadata"]["motion_probe"] == r.item["metadata"]["motion_probe"]
            && item["item_revision"]
                .as_i64()
                .zip(result["item_revision_baselines"][id].as_i64())
                .is_some_and(|(current, original)| original > 0 && current >= original),
        "generated native provenance or receipt baseline changed"
    );
    Ok(())
}

/// Named native history is descriptive only. The retained native owner supplies
/// this envelope; callers must keep that owner and the Motion writer reserved.
pub fn verify_historical(
    command: &CommandEnvelope,
    result: &Value,
    history: &Value,
) -> anyhow::Result<bool> {
    let request: GeneratedOutputRequest =
        serde_json::from_value(command.payload()["request"].clone())?;
    let version = &history["version_snapshot"];
    let native = &history["association_item"];
    let item = json!({"item_id":native["id"],"kind":native["kind"],"metadata":serde_json::from_str::<Value>(native["metadata"].as_str().context("historical native metadata missing")?)?,
        "item_revision":native["native_item_revision"],"retirement":null});
    verify_native_item(command, result, &item)?;
    ensure!(
        history["contract_version"] == 1
            && history["descriptive_only"] == true
            && history["playable"] == false
            && history["catalog_id"] == command.key().catalog_id()
            && history["incarnation"] == command.admission_incarnation()
            && history["generation"]
                .as_i64()
                .is_some_and(|n| n >= command.expected_generation())
            && history["profile"] == request.profile
            && history["version"]["id"] == result["version_id"]
            && history["version"]["association_item_id"] == request.item["id"]
            && version["version_id"] == result["version_id"]
            && version["revision"] == result["version_revision"]
            && version["timeline_id"] == result["timeline_id"]
            && version["timeline_revision"] == result["timeline_revision"]
            && history["timeline"]["id"] == result["timeline_id"]
            && history["timeline"]["edition_item_id"] == result["edition_item_id"]
            && history["timeline_snapshot"]["timeline_id"] == result["timeline_id"]
            && history["timeline_snapshot"]["revision"] == result["timeline_revision"]
            && history["version"]["current_revision"]
                .as_i64()
                .zip(result["version_revision"].as_i64())
                .is_some_and(|(current, original)| original > 0 && current >= original),
        "generated historical identity or receipt snapshot changed"
    );
    let bindings = history["bindings"]
        .as_array()
        .context("historical bindings missing")?;
    ensure!(
        bindings.len() == 1 && request.native.associations.len() == 1,
        "generated historical binding set changed"
    );
    let binding = &bindings[0];
    ensure!(
        binding["profile"] == request.profile
            && binding["file_id"] == request.output.exact.file_id
            && binding["file_revision"] == request.output.exact.revision
            && binding["part"] == 1
            && binding["source_kind"] == "whole_file"
            && binding["source_start_us"] == 0
            && binding["source_end_us"] == request.output_duration_us
            && binding["timeline_start_us"] == 0
            && binding["timeline_end_us"] == request.output_duration_us,
        "generated historical output binding changed"
    );
    let association = result["typed_results"]
        .as_array()
        .context("generated receipt mappings missing")?
        .iter()
        .find(|row| {
            row["owner"] == "association"
                && row["source_association_id"]
                    == request.native.associations[0].source_association_id
        })
        .context("generated receipt association missing")?;
    ensure!(
        binding["association_id"] == association["association_id"]
            && association["item_id"] == request.item["id"]
            && association["file_id"] == request.output.exact.file_id
            && association["file_revision"] == request.output.exact.revision,
        "generated historical association changed"
    );
    let logical = history["logical_retirements"]
        .as_array()
        .context("historical retirement inventory missing")?;
    let typed = &history["retirement"];
    if !typed.is_null() {
        ensure!(
            typed["version_id"] == result["version_id"]
                && typed["version_revision"] == history["version"]["current_revision"],
            "generated typed retirement changed"
        );
    }
    ensure!(
        logical
            .iter()
            .all(|row| row["item_id"] == request.item["id"]
                || row["item_id"] == result["edition_item_id"]),
        "generated logical retirement scope changed"
    );
    let retired = !typed.is_null() || !logical.is_empty();
    ensure!(
        history["retired"].as_bool() == Some(retired),
        "generated positive retirement proof missing"
    );
    Ok(retired)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unrelated_item_head_advance_keeps_exact_provenance_but_never_rewrites_it() {
        let input = json!({"file_id":"input","revision":"input-rev"});
        let exact = json!({"file_id":"output","revision":"output-rev"});
        let model = json!({"contract_version":1,"source_version":{"id":"version","timeline_id":"timeline","revision":1,"origin":"generated"}});
        let probe = json!({"revision":"a".repeat(64)});
        let request = json!({"profile":"default","job_id":"job","attempt":1,"preparation_digest":"b".repeat(64),"provenance":{"recipe":"remux_mp4"},
            "input_version_id":"original","input_version_revision":1,"input_timeline_revision":1,"establish_provisional":false,"input":input,"input_ctime_ns":1,
            "output":{"source_occurrence_id":"output-host","profile":"default","location":"generated","path":"output.mp4","exact":exact,"observed_status":"present"},
            "output_ctime_ns":2,"output_size":3,"output_sha256":"a".repeat(64),"output_duration_us":1000,"output_binding":{},
            "item":{"id":"motion.version:version","kind":"custom:motion_version","metadata":{"motion_version":model,"motion_probe":probe}},
            "native":{"occurrences":[],"associations":[],"mutations":[]},"relationships":[]});
        let command = CommandEnvelope::new(
            1,
            catabolic_core::catalog_command::CommandKey::new("catalog", "motion", "operation")
                .unwrap(),
            "incarnation",
            1,
            &json!({"operation":"generated_output_admit","request":request}).to_string(),
        )
        .unwrap();
        // Independent ancestry is digest-bound to the exact preparation, not
        // reconstructed from whichever source currently occupies the path.
        let admission = json!({"job_id":"job","occurrence":{
            "source":{"library_id":"original-source","profile":"default","native_source":"native-original",
                "catalog":{"catalog_id":"catalog","incarnation":"incarnation","schema_version":40},
                "motion_root":"/original","motion_root_identity":"root","motion_binding_revision":1,
                "native_binding":{},"exclusions":[],"native_policy":{}},
            "native_generation":1,
            "physical":{"source_occurrence_id":"input-host","profile":"default","location":"native-original",
                "path":"input.mp4","exact":input,"observed_status":"present"},
            "motion_file":{"id":"input-host","revision":"motion-revision","generated":0,"fingerprint":"1:2:3:4:0:1"},
            "version_files":[],"association":{"source_association_id":"input-association","source_occurrence_id":"input-host","item_id":"input-owner","part":1}}});
        let mut ancestry_payload = command.payload().clone();
        ancestry_payload["request"]["preparation_digest"] = json!(catabolic_core::sha256(
            serde_json::to_vec(&admission).unwrap()
        ));
        ancestry_payload["request"]["provenance"]["job"] =
            json!({"source_file_id":"input-host","source_revision":"motion-revision"});
        let ancestry_command = CommandEnvelope::new(
            1,
            command.key().clone(),
            "incarnation",
            1,
            &ancestry_payload.to_string(),
        )
        .unwrap();
        let ancestry = input_source(&ancestry_command, &admission).unwrap();
        assert_eq!(ancestry.occurrence.source.library_id, "original-source");
        let mut changed = admission.clone();
        changed["occurrence"]["source"]["native_source"] = json!("replacement");
        assert!(input_source(&ancestry_command, &changed).is_err());
        ancestry_payload["request"]["input_ctime_ns"] = json!(2);
        let changed = CommandEnvelope::new(
            1,
            command.key().clone(),
            "incarnation",
            1,
            &ancestry_payload.to_string(),
        )
        .unwrap();
        assert!(input_source(&changed, &admission).is_err());
        let proof = json!({"job_id":"job","attempt":1,"preparation_digest":"b".repeat(64),"provenance":{"recipe":"remux_mp4"},
            "input_version_id":"original","input_version_revision":1,"input_timeline_revision":1,"input":input,"input_ctime_ns":1,
            "output":exact,"output_ctime_ns":2,"output_size":3,"output_sha256":"a".repeat(64),"output_duration_us":1000});
        let result = json!({"item_revision_baselines":{"motion.version:version":3}});
        let mut item = json!({"item_id":"motion.version:version","kind":"custom:motion_version","retirement":null,"item_revision":3,
            "metadata":{"motion_generated_output":proof,"motion_version":model,"motion_probe":probe}});
        verify_native_item(&command, &result, &item).unwrap();
        item["item_revision"] = json!(4);
        item["metadata"]["annotation"] = json!("later authorized annotation");
        verify_native_item(&command, &result, &item).unwrap();
        item["metadata"]["motion_generated_output"]["output_sha256"] = json!("c".repeat(64));
        assert!(verify_native_item(&command, &result, &item).is_err());
        item["metadata"]["motion_generated_output"] = proof;
        item["item_revision"] = json!(2);
        assert!(verify_native_item(&command, &result, &item).is_err());
        item["item_revision"] = json!(4);
        item["retirement"] = json!({"kind":"retired"});
        assert!(verify_native_item(&command, &result, &item).is_err());
        // Historical verification still requires the same protected metadata,
        // receipt snapshot and exact complete binding, never an absent row.
        let mut payload = command.payload().clone();
        payload["request"]["native"]["associations"] = json!([{"source_association_id":"source-association","source_occurrence_id":"output-host","item_id":"motion.version:version","part":1}]);
        let historical_command = CommandEnvelope::new(
            1,
            command.key().clone(),
            command.admission_incarnation(),
            1,
            &payload.to_string(),
        )
        .unwrap();
        let receipt = json!({"version_id":"version","version_revision":1,"timeline_id":"timeline","timeline_revision":2,"edition_item_id":"motion.edition:edition",
            "item_revision_baselines":{"motion.version:version":3},"typed_results":[{"owner":"association","source_association_id":"source-association","association_id":"actual-association","item_id":"motion.version:version","file_id":"output","file_revision":"output-rev"}]});
        let mut history = json!({"contract_version":1,"descriptive_only":true,"playable":false,"retired":false,"catalog_id":"catalog","incarnation":"incarnation","generation":9,"profile":"default",
            "association_item":{"id":"motion.version:version","kind":"custom:motion_version","metadata":item["metadata"].to_string(),"native_item_revision":4},
            "version":{"id":"version","association_item_id":"motion.version:version","current_revision":3},"version_snapshot":{"version_id":"version","revision":1,"timeline_id":"timeline","timeline_revision":2},
            "timeline":{"id":"timeline","edition_item_id":"motion.edition:edition"},"timeline_snapshot":{"timeline_id":"timeline","revision":2},
            "bindings":[{"profile":"default","file_id":"output","file_revision":"output-rev","part":1,"source_kind":"whole_file","source_start_us":0,"source_end_us":1000,"timeline_start_us":0,"timeline_end_us":1000,"association_id":"actual-association"}],"retirement":null,"logical_retirements":[]});
        assert!(!verify_historical(&historical_command, &receipt, &history).unwrap());
        history["retired"] = json!(true);
        assert!(verify_historical(&historical_command, &receipt, &history).is_err());
        history["retirement"] = json!({"version_id":"version","version_revision":3});
        assert!(verify_historical(&historical_command, &receipt, &history).unwrap());
        history["bindings"][0]["file_revision"] = json!("changed");
        assert!(verify_historical(&historical_command, &receipt, &history).is_err());
    }
}

/// Revalidate a receipt-owned generated version under the retained host. Callers
/// must first establish the exact receipt, output mapping and historical owner;
/// this function adds current selected/strict identity, never a byte grant.
pub fn verify_current_version(
    host: &catabolic_runtime::host::HostSession,
    generation: i64,
    work_item_id: &str,
    command: &CommandEnvelope,
    result: &Value,
    envelope: &Value,
) -> anyhow::Result<()> {
    let identity = host.identity()?;
    let request = catabolic_store::catalog_projection::VersionLineageRequest {
        work_item_id: work_item_id.into(),
        edition_item_id: result["edition_item_id"]
            .as_str()
            .context("generated edition missing")?
            .into(),
        version_item_id: command.payload()["request"]["item"]["id"]
            .as_str()
            .context("generated owner missing")?
            .into(),
        version_id: result["version_id"]
            .as_str()
            .context("generated version missing")?
            .into(),
    };
    let proof = crate::version_proof::read(
        envelope,
        identity["catalog_id"]
            .as_str()
            .context("catalog ID missing")?,
        identity["catalog_incarnation"]
            .as_str()
            .context("catalog incarnation missing")?,
        generation,
        &request,
        crate::version_proof::ProofOrigin::ReceiptVerifiedGenerated,
    )?;
    let current = match proof.kind {
        crate::version_proof::ProofKind::Strict => {
            host.projection_version_manifest(generation, &[request.version_id.clone()])?
        }
        crate::version_proof::ProofKind::Selected => {
            host.projection_selected_current_versions(generation, &[request])?
        }
    };
    ensure!(
        *envelope == current,
        "generated current native version changed"
    );
    Ok(())
}
