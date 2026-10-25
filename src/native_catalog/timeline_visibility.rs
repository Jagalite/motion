//! Descriptive typed timeline identity under a caller-owned authorization writer.
//! Prepared selections must already be validated for the current principal in
//! that writer. No readiness, original occurrence, file or byte grant is made.
use super::{ReadFailure, Runtime};
use anyhow::{Context, ensure};
use catabolic_store::catalog_scope::{ScopeErrorCode, TrustedSelection};
use serde_json::Value;
use sqlx::SqliteConnection;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
pub(crate) enum Visibility {
    Missing,
    Hidden,
    Retired,
    Visible { work_id: String, facts: Value },
}
pub(crate) struct Reader {
    runtime: Arc<Runtime>,
    selections: Vec<TrustedSelection>,
    catalog: String,
    incarnation: String,
    generation: i64,
    timelines: BTreeMap<String, Visibility>,
    captures: BTreeMap<String, Value>,
    bytes: usize,
}
impl Reader {
    pub(crate) async fn new(
        runtime: Arc<Runtime>,
        conn: &mut SqliteConnection,
        selections: Vec<TrustedSelection>,
    ) -> anyhow::Result<Self> {
        runtime.validate_projection(conn).await?;
        let (catalog,incarnation,generation)=sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1").fetch_one(conn).await?;
        Ok(Self {
            runtime,
            selections,
            catalog,
            incarnation,
            generation,
            timelines: BTreeMap::new(),
            captures: BTreeMap::new(),
            bytes: 0,
        })
    }
    fn charge(&mut self, value: &Value) -> anyhow::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(serde_json::to_vec(value)?.len())
            .context("timeline visibility byte overflow")?;
        ensure!(
            self.bytes <= 8 * 1024 * 1024,
            "timeline visibility exceeds complete byte budget"
        );
        Ok(())
    }
    /// Descriptive DTO only: the caller still owns the authorization writer.
    /// No file availability or playback authority follows from this response.
    pub(crate) async fn timeline_body(
        &mut self,
        conn: &mut SqliteConnection,
        id: &str,
    ) -> anyhow::Result<Option<Value>> {
        let Visibility::Visible { work_id, facts } = self.read(conn, id).await? else {
            return Ok(None);
        };
        let edition = facts["edition_metadata"]["motion_edition"]["source_edition"]["id"]
            .as_str()
            .context("timeline edition reference missing")?;
        let bases: Vec<_> = facts["edition_metadata"]["motion_edition"]["source_timelines"]
            .as_array()
            .context("timeline baselines missing")?
            .iter()
            .filter(|base| base["id"] == id)
            .collect();
        ensure!(
            bases.len() == 1,
            "timeline baseline is missing or ambiguous"
        );
        let base = bases[0];
        ensure!(
            base["edition_id"] == edition
                && base.get("order_group_id").is_some_and(Value::is_null)
                && base.get("order_position").is_some_and(Value::is_null),
            "timeline ordering or baseline ownership is unsupported"
        );
        let baseline = base["revision"]
            .as_i64()
            .filter(|v| *v > 0)
            .context("timeline baseline revision missing")?;
        let current = facts["timeline"]["current_revision"]
            .as_i64()
            .filter(|v| *v > 0)
            .context("timeline current revision missing")?;
        let revision = playscale_core::catalog_projection::motion_revision(baseline, current)
            .context("timeline bridged revision overflow")?;
        let timeline = &facts["timeline"];
        let duration = match (
            timeline.get("coverage_start_us"),
            timeline.get("coverage_end_us"),
        ) {
            (Some(Value::Null), Some(Value::Null)) => None,
            (Some(start), Some(end)) if start.as_i64() == Some(0) => {
                let end = end
                    .as_i64()
                    .filter(|v| *v > 0 && *v % 1000 == 0)
                    .context("timeline coverage is not integral milliseconds")?;
                Some(end / 1000)
            }
            _ => anyhow::bail!("timeline coverage is unsupported"),
        };
        let capture = self
            .captures
            .get(&work_id)
            .context("complete timeline work capture missing")?;
        let versions = capture["versions"]
            .as_array()
            .context("complete version inventory missing")?;
        let heads = capture["version_heads"]
            .as_array()
            .context("complete version heads missing")?;
        let retirements = capture["version_retirements"]
            .as_array()
            .context("version retirement inventory missing")?;
        let mut expected = std::collections::BTreeSet::new();
        for version in versions {
            let version_id = version["id"].as_str().context("version identity missing")?;
            let matching: Vec<_> = heads
                .iter()
                .filter(|head| {
                    head["version_id"] == version_id
                        && head["revision"] == version["current_revision"]
                })
                .collect();
            ensure!(
                matching.len() == 1,
                "current version head is ambiguous or missing"
            );
            if matching[0]["timeline_id"] == id {
                ensure!(
                    !retirements.iter().any(|r| r["version_id"] == version_id),
                    "retired version timeline shape is unsupported"
                );
                ensure!(
                    expected.insert(version_id.to_owned()),
                    "duplicate timeline version"
                );
            }
        }
        let mut actual = std::collections::BTreeSet::new();
        for version in facts["versions"]
            .as_array()
            .context("named timeline versions missing")?
        {
            let version_id = version["id"]
                .as_str()
                .context("named version identity missing")?;
            ensure!(
                version["retired"] == 0 && actual.insert(version_id.to_owned()),
                "named timeline contains retired or duplicate version"
            );
        }
        ensure!(
            actual == expected,
            "named and complete timeline version inventories differ"
        );
        Ok(Some(
            serde_json::json!({"id":id,"revision":revision.to_string(),
            "item_id":work_id,"edition_id":edition,"duration_ms":duration,
            "version_ids":actual,"order_group_id":null,"order_position":null}),
        ))
    }
    pub(crate) async fn read(
        &mut self,
        conn: &mut SqliteConnection,
        id: &str,
    ) -> anyhow::Result<Visibility> {
        let current:(String,String,i64)=sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1").fetch_one(&mut *conn).await?;
        ensure!(
            current.0 == self.catalog
                && current.1 == self.incarnation
                && current.2 == self.generation,
            "timeline visibility snapshot changed"
        );
        if let Some(value) = self.timelines.get(id) {
            return Ok(value.clone());
        }
        ensure!(
            self.timelines.len() < 10000,
            "timeline visibility identity bound"
        );
        let hint:Option<(String,String)>=sqlx::query_as("SELECT t.edition_id,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?").bind(id).fetch_optional(&mut *conn).await?;
        let Some((edition, work)) = hint else {
            return Ok(Visibility::Missing);
        };
        let native_edition = format!("motion.edition:{edition}");
        let generation = self.generation;
        let requested = id.to_owned();
        let owner = native_edition.clone();
        let named = self
            .runtime
            .with_host(move |host| {
                host.projection_timeline(generation, &requested, &owner)
                    .map_err(Into::into)
            })
            .await?;
        ensure!(
            named["catalog_id"] == self.catalog
                && named["incarnation"] == self.incarnation
                && named["generation"] == generation
                && named["contract_version"] == 1
                && named["descriptive_only"] == true
                && named["playable"] == false
                && named["timeline"]["id"] == id
                && named["timeline"]["edition_item_id"] == native_edition
                && named["edition"]["id"] == native_edition
                && named["edition_metadata"]["motion_edition"]["contract_version"] == 1
                && named["edition_metadata"]["motion_edition"]["source_edition"]["id"] == edition
                && named["edition_metadata"]["motion_edition"]["source_edition"]["item_id"] == work,
            "timeline compatibility hint differs from exact native owner"
        );
        if named["active"] == false && named["edition_retirement"]["item_id"] == native_edition {
            self.timelines.insert(id.into(), Visibility::Retired);
            return Ok(Visibility::Retired);
        }
        ensure!(
            named["active"] == true && named["edition_retirement"].is_null(),
            "timeline retirement state is unsupported"
        );
        let visible = if self.selections.is_empty() {
            false
        } else {
            match self
                .runtime
                .read_projectable_items(conn, self.selections.clone(), Some(work.clone()))
                .await
            {
                Ok(rows) => {
                    ensure!(
                        rows.len() <= 1 && rows.first().is_none_or(|r| r[0] == work),
                        "timeline work visibility result changed"
                    );
                    !rows.is_empty()
                }
                Err(error)
                    if error
                        .downcast_ref::<ReadFailure>()
                        .is_some_and(|e| e.0 == ScopeErrorCode::NotFoundOrNotVisible) =>
                {
                    false
                }
                Err(error) => return Err(error),
            }
        };
        if !visible {
            self.timelines.insert(id.into(), Visibility::Hidden);
            return Ok(Visibility::Hidden);
        }
        if !self.captures.contains_key(&work) {
            let wanted = work.clone();
            let capture = self
                .runtime
                .with_host(move |host| {
                    let capture = host.capture_work_dependencies(generation, &wanted)?;
                    Ok((
                        capture.catalog_id().to_owned(),
                        capture.incarnation().to_owned(),
                        capture.dependencies().clone(),
                    ))
                })
                .await?;
            ensure!(
                capture.0 == self.catalog && capture.1 == self.incarnation,
                "timeline complete work capture identity changed"
            );
            self.charge(&capture.2)?;
            self.captures.insert(work.clone(), capture.2);
        }
        let capture = &self.captures[&work];
        let timelines: Vec<_> = capture["timelines"]
            .as_array()
            .context("timeline inventory missing")?
            .iter()
            .filter(|t| t["id"] == id)
            .collect();
        let heads: Vec<_> = capture["timeline_heads"]
            .as_array()
            .context("timeline heads missing")?
            .iter()
            .filter(|t| t["timeline_id"] == id)
            .collect();
        let edges: Vec<_> = capture["relationships"]
            .as_array()
            .context("timeline relationships missing")?
            .iter()
            .filter(|e| {
                e["target_id"] == native_edition
                    && e["kind"] == "custom:motion_edition"
                    && e["active"] == 1
            })
            .collect();
        ensure!(
            timelines.len() == 1
                && heads.len() == 1
                && edges.len() == 1
                && edges[0]["source_id"] == work
                && timelines[0]["edition_item_id"] == native_edition
                && timelines[0]["current_revision"] == named["timeline"]["current_revision"]
                && heads[0]["revision"] == timelines[0]["current_revision"],
            "timeline current ownership is detached, ambiguous or changed"
        );
        self.charge(&named)?;
        let result = Visibility::Visible {
            work_id: work,
            facts: named,
        };
        self.timelines.insert(id.into(), result.clone());
        Ok(result)
    }
}
