//! Pure assembly from captured native owners and adapter-validated completion.
//! This creates no authority: the publication owner must still authorize the
//! current preparation, reserve its disposition and retain real output custody.
use super::PublicationPlan;
use anyhow::{Context, ensure};
use catabolic_store::{
    catalog_generated_output::{GeneratedInputFacts, GeneratedOutputRequest},
    catalog_identity_mutation::{IdentityMutation, TimelineState},
    catalog_import_typed::{ImportedAssociation, ImportedRelationship, NativeIdentityBatch},
};
use serde_json::{Value, json};

fn text<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("generated publication {key} missing"))
}
fn recorded_ctime(stamp: &str) -> anyhow::Result<i64> {
    let parts: Vec<_> = stamp.split(':').collect();
    ensure!(
        parts.len() == 6,
        "generated input lacks complete fingerprint"
    );
    let seconds: i64 = parts[4].parse()?;
    let nanos: i64 = parts[5].parse()?;
    ensure!(
        (0..1_000_000_000).contains(&nanos),
        "invalid recorded ctime"
    );
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|s| s.checked_add(nanos))
        .context("recorded ctime overflow")
}

pub(super) fn assemble(
    plan: &PublicationPlan,
    input: &GeneratedInputFacts,
    output: &crate::native_generated::GeneratedPhysicalFacts,
    probe: &Value,
) -> anyhow::Result<GeneratedOutputRequest> {
    let saved = &plan.source["admission"];
    let version = &input.original.version;
    let timeline_revision = input
        .timeline
        .expected_revision
        .context("missing current native timeline revision")?;
    ensure!(
        saved["catalog_id"] == input.catalog_id
            && saved["incarnation"] == input.incarnation
            && saved["version_id"] == version.version_id
            && saved["timeline_id"] == version.timeline_id
            && saved["native_version_revision"].as_i64() == Some(input.version_revision)
            && saved["native_timeline_revision"].as_i64() == Some(timeline_revision),
        "generated preparation native owners changed"
    );
    let occurrence: motion_catalog_backend::occurrence_bridge::OccurrenceMapping =
        serde_json::from_value(saved["occurrence"].clone())?;
    ensure!(
        version.bindings.len() == 1
            && version.bindings[0].file == occurrence.physical.exact
            && output.output.profile == input.profile
            && occurrence.physical.profile == input.profile,
        "generated preparation exact original changed"
    );
    let coverage = version
        .declared_coverage
        .context("generated input coverage missing")?;
    let exact_coverage = json!({"start_us":0,"end_us":output.output_duration_us});
    ensure!(
        serde_json::to_value(coverage)? == exact_coverage
            && input.timeline.coverage == Some(coverage)
            && version.bindings[0].timeline_interval == Some(coverage)
            && serde_json::to_value(&version.bindings[0].source)?
                == json!({"kind":"whole_file","duration_us":output.output_duration_us}),
        "generated first cohort requires exact measured whole-source coverage"
    );
    let ids = &plan.source["ids"];
    let version_id = text(ids, "version")?;
    let file_id = text(ids, "file")?;
    let association_id = text(ids, "association")?;
    let binding_id = text(ids, "binding")?;
    let evidence_id = text(ids, "evidence")?;
    ensure!(
        output.output.source_occurrence_id == file_id,
        "generated output ID changed"
    );
    let item_id = format!("motion.version:{version_id}");
    let establish = input.timeline.identity_state == TimelineState::Provisional;
    let target_timeline = timeline_revision
        .checked_add(i64::from(establish))
        .context("timeline revision exhausted")?;
    let target_original = input
        .version_revision
        .checked_add(i64::from(establish))
        .context("original revision exhausted")?;
    let job = &plan.source["job"];
    ensure!(
        job["id"] == plan.job_id
            && job["attempt"].as_i64() == Some(plan.attempt)
            && matches!(
                text(job, "recipe")?,
                "remux_mp4" | "audio_aac" | "h264720p" | "video_profile"
            ),
        "generated preparation identity or recipe unsupported"
    );
    let provenance = json!({"contract_version":1,"job":job,"recipe_digest":saved["recipe_digest"],"tracks":saved["tracks"],
        "disposition":"validated_motion_preparation","input_version_id":version.version_id,
        "input_version_revision":input.version_revision,"input_timeline_revision":timeline_revision});
    // This evidence is an explicit declaration of the validated transform and
    // exact snapshots. Equal duration alone never supplies the effect grant.
    let claims = json!([
        {"edition_item_id":version.edition_item_id,"timeline_id":version.timeline_id,"timeline_revision":target_timeline,"version_id":version_id,"version_revision":1,"exact_files":[output.output.exact]},
        {"edition_item_id":version.edition_item_id,"timeline_id":version.timeline_id,"timeline_revision":target_timeline,"version_id":version.version_id,"version_revision":target_original,"exact_files":[occurrence.physical.exact]}
    ]);
    let timelines = if establish {
        json!([{ "timeline_id":version.timeline_id,"edition_item_id":version.edition_item_id,
        "expected_revision":timeline_revision,"identity_state":"established","coverage":coverage }])
    } else {
        json!([])
    };
    let mutation: IdentityMutation = serde_json::from_value(
        json!({"contract_version":1,"profile":input.profile,"context":input.context,
        "timelines":timelines,"versions":[{"expected_revision":null,"expected_timeline_revision":target_timeline,
          "version":{"contract_version":1,"version_id":version_id,"association_item_id":item_id,"edition_item_id":version.edition_item_id,
            "timeline_id":version.timeline_id,"state":"ready","declared_coverage":coverage,
            "bindings":[{"binding_id":binding_id,"association_id":association_id,"file":output.output.exact,"part":1,
              "source":{"kind":"whole_file","duration_us":output.output_duration_us},"timeline_interval":coverage}]},
          "equivalence":{"basis":"declared","evidence_id":evidence_id},"expected_binding_revisions":{(binding_id):null}}],
        "evidence":[{"id":evidence_id,"basis":"declared","claims":claims,"evidence":provenance}]}),
    )?;
    Ok(GeneratedOutputRequest {
        profile: input.profile.clone(),
        job_id: plan.job_id.clone(),
        attempt: plan.attempt,
        preparation_digest: catabolic_core::sha256(serde_json::to_vec(saved)?),
        provenance,
        input_version_id: version.version_id.clone(),
        input_version_revision: input.version_revision,
        input_timeline_revision: timeline_revision,
        establish_provisional: establish,
        input: occurrence.physical.exact,
        input_ctime_ns: recorded_ctime(text(&occurrence.motion_file, "fingerprint")?)?,
        output: output.output.clone(),
        output_ctime_ns: output.output_ctime_ns,
        output_size: output.output_size,
        output_sha256: output.output_sha256.clone(),
        output_duration_us: output.output_duration_us,
        output_binding: output.output_binding.clone(),
        item: json!({"id":item_id,"kind":"custom:motion_version","metadata":{"motion_version":{"contract_version":1,
            "source_version":{"id":version_id,"timeline_id":version.timeline_id,"revision":1,"label":"Generated","origin":"generated","equivalence":"declared"}},"motion_probe":probe}}),
        native: NativeIdentityBatch {
            occurrences: vec![output.output.clone()],
            associations: vec![ImportedAssociation {
                source_association_id: association_id.into(),
                source_occurrence_id: file_id.into(),
                item_id: item_id.clone(),
                part: 1,
            }],
            mutations: vec![serde_json::to_value(mutation)?],
        },
        relationships: vec![ImportedRelationship {
            source_relationship_id: text(ids, "relationship")?.into(),
            source_revision: 1,
            source_item_id: version.edition_item_id.clone(),
            target_item_id: item_id,
            kind: "custom:motion_version".into(),
            position: None,
            metadata: json!({}),
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        PublicationPlan,
        GeneratedInputFacts,
        crate::native_generated::GeneratedPhysicalFacts,
    ) {
        let exact = json!({"file_id":"native-input","revision":"native-input-revision"});
        let interval = json!({"start_us":0,"end_us":1_000_000});
        let source = json!({"library_id":"source","profile":"default","native_source":"native-source",
            "catalog":{"catalog_id":"catalog","incarnation":"incarnation","schema_version":39},
            "motion_root":"/fixture","motion_root_identity":"identity","motion_binding_revision":1,
            "native_binding":{},"exclusions":[],"native_policy":{}});
        let physical = json!({"source_occurrence_id":"input","profile":"default","location":"native-source","path":"input.mp4","exact":exact,"observed_status":"present"});
        let input:GeneratedInputFacts=serde_json::from_value(json!({"catalog_id":"catalog","incarnation":"incarnation","generation":20,"profile":"default",
            "context":{"id":"context","authority_namespace":"motion","context_key":"work","expected_revision":1},
            "original":{"version":{"contract_version":1,"version_id":"original","association_item_id":"motion.version:original","edition_item_id":"motion.edition:edition",
              "timeline_id":"timeline","state":"ready","declared_coverage":interval,
              "bindings":[{"binding_id":"input-binding","association_id":"input-association","file":exact,"part":1,"source":{"kind":"whole_file","duration_us":1_000_000},"timeline_interval":interval}]},"equivalence":null},
            "version_revision":1,"timeline":{"timeline_id":"timeline","edition_item_id":"motion.edition:edition","expected_revision":1,"identity_state":"provisional","coverage":interval},
            "binding_revisions":{"input-binding":1}})).unwrap();
        let plan = PublicationPlan {
            operation_id: "operation".into(),
            job_id: "job".into(),
            attempt: 2,
            output_relative: "output.mp4".into(),
            source: json!({"admission":{"catalog_id":"catalog","incarnation":"incarnation","version_id":"original","timeline_id":"timeline",
                "native_version_revision":1,"native_timeline_revision":1,"recipe_digest":"digest","tracks":{},
                "occurrence":{"source":source,"native_generation":19,"physical":physical,"motion_file":{"id":"input","revision":"hash","fingerprint":"10:20:30:40:50:60"},"version_files":[],
                  "association":{"source_association_id":"input-association","source_occurrence_id":"input","item_id":"motion.version:original","part":1}}},
                "job":{"id":"job","attempt":2,"recipe":"remux_mp4","backend":"software"},
                "ids":{"file":"output","version":"output-version","association":"output-association","binding":"output-binding","evidence":"output-evidence","relationship":"output-relationship"}}),
        };
        let output=crate::native_generated::GeneratedPhysicalFacts {
            output:serde_json::from_value(json!({"source_occurrence_id":"output","profile":"default","location":"generated","path":"output.mp4","exact":{"file_id":"native-output","revision":"native-output-revision"},"observed_status":"present"})).unwrap(),
            output_ctime_ns:100,output_size:20,output_sha256:"a".repeat(64),output_duration_us:1_000_000,output_binding:json!({}),
            output_file:std::sync::Arc::new(tempfile::tempfile().unwrap()) };
        (plan, input, output)
    }
    #[test]
    fn pure_assembly_pins_exact_transform_and_never_guesses_coverage() {
        let (plan, input, mut output) = fixture();
        let request = assemble(&plan, &input, &output, &json!({})).unwrap();
        assert!(request.establish_provisional);
        let mutation = &request.native.mutations[0];
        assert_eq!(mutation["timelines"][0]["expected_revision"], 1);
        assert_eq!(mutation["versions"][0]["expected_timeline_revision"], 2);
        assert_eq!(mutation["evidence"][0]["claims"][1]["version_revision"], 2);
        assert_eq!(request.input_ctime_ns, 50_000_000_060);
        assert_eq!(
            request.item["metadata"]["motion_version"]["source_version"]["origin"],
            "generated"
        );
        output.output_duration_us = 999_000;
        assert!(assemble(&plan, &input, &output, &json!({})).is_err());
        output.output_duration_us = 1_000_000;
        let mut changed = input.clone();
        changed.version_revision += 1;
        assert!(assemble(&plan, &changed, &output, &json!({})).is_err());
        let mut changed = plan.clone();
        changed.source["job"]["recipe"] = json!("image_preview");
        assert!(assemble(&changed, &input, &output, &json!({})).is_err());
    }
    #[test]
    fn complete_recorded_ctime_is_checked_without_guessing() {
        assert_eq!(recorded_ctime("10:20:30:40:50:60").unwrap(), 50_000_000_060);
        for bad in [
            "10:20",
            "10:20:30:40:50:1000000000",
            "10:20:30:40:9223372036854775807:0",
        ] {
            assert!(recorded_ctime(bad).is_err());
        }
    }
}
