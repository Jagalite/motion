//! Exact retained artwork references and non-secret HTTP acknowledgments.
//! This audit never authorizes asset disclosure or replays a native effect.
use anyhow::{Context, ensure};
use catabolic_store::catalog_import::ImportBatch;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const TABLES: &[&str] = &[
    "artwork_assets",
    "artwork_contributions",
    "artwork_selections",
    "item_artwork_assets",
];

pub(crate) async fn qualify(
    db: &mut SqliteConnection,
    batches: &[ImportBatch],
) -> anyhow::Result<(bool, u64)> {
    let mut owners = BTreeSet::new();
    let mut states = BTreeMap::new();
    for batch in batches {
        for item in &batch.items {
            owners.insert(item.item_id.clone());
        }
        if let Some(typed) = &batch.typed {
            for metadata in &typed.metadata {
                for source in &metadata.contributions {
                    if source.namespace == "motion.artwork"
                        && source.record_id == "source_projection"
                    {
                        ensure!(
                            states
                                .insert(
                                    metadata.item_id.clone(),
                                    source.fields["motion_artwork"].clone()
                                )
                                .is_none(),
                            "duplicate imported artwork state"
                        );
                    }
                }
            }
        }
    }
    let mut referenced = BTreeMap::new();
    let mut ownership = BTreeMap::<String, BTreeSet<String>>::new();
    for (item, state) in &states {
        ensure!(owners.contains(item), "artwork source lacks imported owner");
        playscale_core::artwork::native::ReferenceState::parse_json(
            &serde_json::to_string(state)?,
            item,
        )?;
        ensure!(
            crate::motion_export::artwork_projection(db, item)
                .await?
                .as_ref()
                == Some(state),
            "artwork source projection changed"
        );
        for asset in state["assets"]
            .as_array()
            .context("artwork asset closure missing")?
        {
            let id = asset["asset_id"]
                .as_str()
                .context("artwork asset identity missing")?;
            ownership
                .entry(item.clone())
                .or_default()
                .insert(id.to_owned());
            if let Some(prior) = referenced.insert(id.to_owned(), asset.clone()) {
                ensure!(prior == *asset, "conflicting artwork asset descriptors");
            }
        }
    }
    // Every retained reference must be represented by its exact typed subject.
    let mut qualified = true;
    for table in [
        "artwork_contributions",
        "artwork_selections",
        "item_artwork_assets",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&mut *db)
            .await?;
        ensure!(count <= 10000, "artwork reference audit exceeds row budget");
        let ids: Vec<String> = sqlx::query_scalar(&format!("SELECT DISTINCT item_id FROM {table}"))
            .fetch_all(&mut *db)
            .await?;
        qualified &= ids.iter().all(|id| states.contains_key(id));
    }
    let assets: Vec<String> = sqlx::query_scalar("SELECT id FROM artwork_assets LIMIT 10001")
        .fetch_all(&mut *db)
        .await?;
    ensure!(
        assets.len() <= 10000,
        "artwork asset audit exceeds row budget"
    );
    qualified &= assets.iter().all(|id| referenced.contains_key(id));
    let receipts=sqlx::query("SELECT operation,target,status,body,issued_nonce IS NULL AS no_nonce FROM idempotency_records WHERE operation IN ('createCatalogItem','uploadArtwork') AND length(CAST(body AS BLOB))<=65536 LIMIT 10001").fetch_all(&mut *db).await?;
    ensure!(
        receipts.len() <= 10000,
        "artwork receipt audit exceeds row budget"
    );
    let mut count = 0;
    for row in receipts {
        if row.get::<i64, _>("status") != 201 || row.get::<i64, _>("no_nonce") != 1 {
            continue;
        }
        let Some(raw) = row.get::<Option<String>, _>("body") else {
            continue;
        };
        let Ok(body) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if qualified_receipt(
            row.get("operation"),
            row.get("target"),
            &body,
            &owners,
            &referenced,
            &ownership,
        ) {
            count += 1;
        }
    }
    Ok((qualified, count))
}
fn keys(body: &Value, expected: &[&str]) -> bool {
    body.as_object().is_some_and(|o| {
        o.len() == expected.len() && expected.iter().all(|key| o.contains_key(*key))
    })
}
fn qualified_receipt(
    operation: &str,
    target: &str,
    body: &Value,
    owners: &BTreeSet<String>,
    assets: &BTreeMap<String, Value>,
    ownership: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    match operation {
        "createCatalogItem" => {
            target == "-"
                && keys(
                    body,
                    &[
                        "id",
                        "revision",
                        "kind",
                        "title",
                        "library_ids",
                        "availability",
                        "external_ids",
                        "artwork_id",
                        "default_timeline_id",
                        "match_state",
                        "parent_ids",
                    ],
                )
                && body["id"].as_str().is_some_and(|id| owners.contains(id))
                && body["revision"]
                    .as_str()
                    .is_some_and(|n| n.parse::<u64>().is_ok_and(|n| n > 0))
                && matches!(
                    body["kind"].as_str(),
                    Some("movie" | "series" | "unclassified")
                )
                && body["title"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 1024)
                && body["library_ids"] == json!([])
                && body["parent_ids"] == json!([])
                && body["external_ids"] == json!([])
                && body["artwork_id"].is_null()
                && body["default_timeline_id"].is_null()
                && body["availability"] == "unknown"
                && matches!(body["match_state"].as_str(), Some("unmatched" | "manual"))
        }
        "uploadArtwork" => {
            let Some(id) = body["id"].as_str() else {
                return false;
            };
            let Some(asset) = assets.get(id) else {
                return false;
            };
            owners.contains(target)
                && ownership.get(target).is_some_and(|ids| ids.contains(id))
                && asset["content_sha256"] == id
                && body
                    == &json!({"id":id,"revision":"1","digest":format!("sha256:{id}"),"kind":"artwork","mime_type":asset["mime"],"size_bytes":asset["size_bytes"].as_i64().map(|n|n.to_string()),"content_url":format!("/api/v2/media/assets/{id}/content?revision=1"),"source_revision":null})
        }
        _ => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_artwork_receipts_require_exact_owner_asset_and_shape() {
        let owners = BTreeSet::from(["work".to_owned()]);
        let assets = BTreeMap::from([(
            "asset".to_owned(),
            json!({"mime":"image/png","size_bytes":9,"content_sha256":"asset"}),
        )]);
        let ownership = BTreeMap::from([("work".to_owned(), BTreeSet::from(["asset".to_owned()]))]);
        let body = json!({"id":"asset","revision":"1","digest":"sha256:asset","kind":"artwork","mime_type":"image/png","size_bytes":"9","content_url":"/api/v2/media/assets/asset/content?revision=1","source_revision":null});
        assert!(qualified_receipt(
            "uploadArtwork",
            "work",
            &body,
            &owners,
            &assets,
            &ownership
        ));
        assert!(!qualified_receipt(
            "uploadArtwork",
            "foreign",
            &body,
            &owners,
            &assets,
            &ownership
        ));
        assert!(!qualified_receipt(
            "unknown", "work", &body, &owners, &assets, &ownership
        ));
        let mut both = owners.clone();
        both.insert("other".into());
        assert!(!qualified_receipt(
            "uploadArtwork",
            "other",
            &body,
            &both,
            &assets,
            &ownership
        ));
        let mut changed = body.clone();
        changed["digest"] = json!("sha256:other");
        assert!(!qualified_receipt(
            "uploadArtwork",
            "work",
            &changed,
            &owners,
            &assets,
            &ownership
        ));
        let mut changed = body;
        changed["unreviewed_reference"] = json!("foreign");
        assert!(!qualified_receipt(
            "uploadArtwork",
            "work",
            &changed,
            &owners,
            &assets,
            &ownership
        ));
    }
}
