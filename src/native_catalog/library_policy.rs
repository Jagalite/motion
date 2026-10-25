//! Exact current native library definitions for Motion-owned access grants.
//! Call only under the existing authorization/write transaction; no saved query
//! is created and no compatibility library row supplies policy authority.
use super::Runtime;
use anyhow::{Context, ensure};
use std::collections::BTreeSet;

impl Runtime {
    pub(crate) async fn validate_library_definitions(
        &self,
        conn: &mut sqlx::SqliteConnection,
        ids: &BTreeSet<String>,
    ) -> anyhow::Result<bool> {
        ensure!(ids.len() <= 200, "native policy library limit exceeded");
        let identity: (String, String) = sqlx::query_as(
            "SELECT json_extract(identity_json,'$.catalog_id'),json_extract(identity_json,'$.incarnation') FROM catabolic_binding WHERE id=1",
        )
        .fetch_one(conn)
        .await?;
        let requested = ids.clone();
        self.with_host(move |host| {
            let current = host.identity()?;
            ensure!(
                current["catalog_id"] == identity.0 && current["catalog_incarnation"] == identity.1,
                "native access policy catalog identity changed"
            );
            let native_ids: Vec<String> = requested
                .iter()
                .map(|id| format!("motion.library:{id}"))
                .collect();
            // No projected scalar is consumed. Pin the current native generation
            // inside the same retained-owner call rather than requiring a cache
            // refresh (which would invert the caller's writer ownership).
            let generation = host.catalog_generation()?;
            let facts = host.projection_named_items(generation, &native_ids)?;
            ensure!(
                facts["catalog_id"] == identity.0
                    && facts["incarnation"] == identity.1
                    && facts["generation"].as_i64() == Some(generation),
                "native policy definition fence changed"
            );
            let missing = facts["missing_ids"]
                .as_array()
                .context("native missing definition list absent")?;
            let mut known = BTreeSet::new();
            for row in facts["items"]
                .as_array()
                .context("native definition items absent")?
            {
                if !row["retirement"].is_null() {
                    continue;
                }
                let definition = &row["metadata"]["motion_library"];
                let id = definition["id"]
                    .as_str()
                    .context("native library definition identity missing")?;
                ensure!(
                    requested.contains(id)
                        && definition["contract_version"] == 1
                        && row["item_id"] == format!("motion.library:{id}")
                        && row["kind"] == "custom:motion_library"
                        && row["item_revision"].as_i64().is_some_and(|r| r > 0),
                    "native library definition identity mismatch"
                );
                ensure!(
                    known.insert(id.to_owned()),
                    "duplicate native library definition"
                );
            }
            Ok(missing.is_empty() && known == requested)
        })
        .await
    }
}
