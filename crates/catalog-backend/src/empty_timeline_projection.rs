//! Descriptive zero-version timelines. No file, playable version, readiness or
//! history reassignment is inferred from an edition metadata marker.
use anyhow::{Context, ensure};
use serde_json::{Value, json};

pub(crate) fn row(
    identity: &Value,
    generation: i64,
    item: &Value,
    baseline: &Value,
    facts: &Value,
) -> anyhow::Result<Option<Value>> {
    let model = &item["metadata"]["motion_edition"];
    ensure!(
        model["contract_version"] == 1 && model["origin"] == "native_empty_edition",
        "unsupported empty edition origin contract"
    );
    let edition = baseline["edition_id"]
        .as_str()
        .context("empty timeline edition missing")?;
    let timeline = baseline["id"]
        .as_str()
        .context("empty timeline ID missing")?;
    ensure!(
        facts["contract_version"] == 1
            && facts["catalog_id"] == identity["catalog_id"]
            && facts["incarnation"] == identity["catalog_incarnation"]
            && facts["generation"].as_i64() == Some(generation)
            && facts["active"] == true
            && facts["edition_retirement"].is_null()
            && facts["descriptive_only"] == true
            && facts["playable"] == false
            && item["retirement"].is_null()
            && item["item_id"] == format!("motion.edition:{edition}")
            && facts["edition"]["id"] == item["item_id"]
            && facts["edition"]["native_item_revision"] == item["item_revision"]
            && facts["edition_metadata"]["motion_edition"] == *model
            && facts["timeline"]["id"] == timeline
            && facts["timeline"]["edition_item_id"] == item["item_id"],
        "empty timeline current owner/fence changed"
    );
    let context = &model["editorial_context"];
    let contexts = facts["revision_contexts"]
        .as_array()
        .context("timeline context provenance missing")?;
    ensure!(
        contexts.iter().any(|c| c["id"] == context["id"]
            && c["authority_namespace"] == context["authority_namespace"]
            && c["context_key"] == context["context_key"]
            && c["current_revision"].as_i64().is_some_and(|n| n > 0)
            && c["timeline_command_context_revision"]
                .as_i64()
                .is_some_and(|n| n > 0)),
        "empty timeline typed context changed"
    );
    if !facts["versions"]
        .as_array()
        .context("complete native timeline versions missing")?
        .is_empty()
    {
        return Ok(None);
    }
    let current = &facts["timeline"];
    ensure!(
        matches!(
            current["identity_state"].as_str(),
            Some("provisional" | "established")
        ),
        "unsupported descriptive timeline state"
    );
    let head = current["current_revision"]
        .as_i64()
        .filter(|n| *n > 0)
        .context("current timeline revision missing")?;
    let base = baseline["revision"]
        .as_i64()
        .filter(|n| *n > 0)
        .context("timeline baseline revision missing")?;
    // The native_empty_edition owner creates this typed timeline at native1 in
    // its receipt transaction; the protected source baseline is retained.
    ensure!(base == 1, "empty edition baseline changed");
    let duration = if current["coverage_start_us"].is_null() && current["coverage_end_us"].is_null()
    {
        Value::Null
    } else {
        let start = current["coverage_start_us"]
            .as_i64()
            .context("timeline coverage start missing")?;
        let end = current["coverage_end_us"]
            .as_i64()
            .context("timeline coverage end missing")?;
        ensure!(
            start == 0 && end >= 0 && end % 1000 == 0,
            "timeline coverage is not exact Motion milliseconds"
        );
        json!(end / 1000)
    };
    let mut result = baseline.clone();
    result["revision"] = json!(
        playscale_core::catalog_projection::motion_revision(base, head)
            .context("timeline revision overflow")?
    );
    result["duration_ms"] = duration;
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Value, Value, Value, Value) {
        let identity = json!({"catalog_id":"catalog","catalog_incarnation":"incarnation"});
        let timeline = json!({"id":"edition","edition_id":"edition","revision":1,"duration_ms":null,"order_group_id":null,"order_position":null});
        let model = json!({"contract_version":1,"origin":"native_empty_edition","source_timelines":[timeline.clone()],"editorial_context":{"id":"context","authority_namespace":"host","context_key":"key"}});
        let item = json!({"item_id":"motion.edition:edition","item_revision":1,"retirement":null,"metadata":{"motion_edition":model}});
        let facts = json!({"contract_version":1,"catalog_id":"catalog","incarnation":"incarnation","generation":7,"active":true,"edition_retirement":null,"descriptive_only":true,"playable":false,"edition":{"id":"motion.edition:edition","native_item_revision":1},"edition_metadata":item["metadata"],"timeline":{"id":"edition","edition_item_id":"motion.edition:edition","identity_state":"provisional","current_revision":1,"coverage_start_us":null,"coverage_end_us":null},"versions":[],"revision_contexts":[{"id":"context","authority_namespace":"host","context_key":"key","current_revision":1,"timeline_command_context_revision":1}]});
        (identity, item, timeline, facts)
    }
    #[test]
    fn descriptive_current_head_not_frozen_baseline_and_no_playable_proof() {
        let (identity, item, baseline, mut facts) = fixture();
        assert_eq!(
            row(&identity, 7, &item, &baseline, &facts).unwrap(),
            Some(baseline.clone())
        );
        facts["timeline"]["current_revision"] = json!(2);
        facts["timeline"]["coverage_start_us"] = json!(0);
        facts["timeline"]["coverage_end_us"] = json!(125_000);
        facts["revision_contexts"][0]["current_revision"] = json!(2);
        facts["revision_contexts"][0]["timeline_command_context_revision"] = json!(2);
        let result = row(&identity, 7, &item, &baseline, &facts)
            .unwrap()
            .unwrap();
        assert_eq!(result["revision"], 2);
        assert_eq!(result["duration_ms"], 125);
        assert!(result.get("ready").is_none());
        assert!(result.get("file_id").is_none());
        assert_eq!(baseline["revision"], 1);
        facts["versions"] = json!([{"id":"version"}]);
        assert!(
            row(&identity, 7, &item, &baseline, &facts)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn ambiguous_or_retired_timeline_never_materializes_current_description() {
        let (identity, item, baseline, facts) = fixture();
        for (field, value) in [
            ("generation", json!(8)),
            ("active", json!(false)),
            ("playable", json!(true)),
            ("revision_contexts", json!([])),
            ("edition_retirement", json!({"kind":"split"})),
        ] {
            let mut bad = facts.clone();
            bad[field] = value;
            assert!(
                row(&identity, 7, &item, &baseline, &bad).is_err(),
                "{field}"
            );
        }
        let mut bad = facts.clone();
        bad["timeline"]["coverage_start_us"] = json!(0);
        bad["timeline"]["coverage_end_us"] = json!(1001);
        assert!(row(&identity, 7, &item, &baseline, &bad).is_err());
        let mut bad = facts.clone();
        bad["edition"]["native_item_revision"] = json!(2);
        assert!(row(&identity, 7, &item, &baseline, &bad).is_err());
    }
}
