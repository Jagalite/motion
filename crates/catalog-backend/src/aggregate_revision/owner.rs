//! Trusted capture-to-issuance bridge. No caller-supplied dependency JSON enters
//! the issuer. The embedding owner must retain this host and transaction through
//! commit, and must not expose the returned token before that commit succeeds.
use anyhow::{Context, ensure};
use catabolic_runtime::host::HostSession;
use catabolic_store::catalog_scope::SavedQueryRef;
use playscale_core::catalog_aggregate_revision::{
    CompleteWorkSnapshot, Dependency, DependencyKind, IssuedRevision,
};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::collections::{BTreeMap, BTreeSet};

pub async fn issue_work_in(
    tx: &mut Transaction<'_, Sqlite>,
    host: &mut HostSession,
    work_id: &str,
) -> anyhow::Result<IssuedRevision> {
    let (baseline, snapshot, _) = capture_work_in(tx, host, work_id).await?;
    super::issue_in(tx, baseline, &snapshot).await
}

/// Complete native membership, including libraries hidden from the principal.
/// Visibility must be checked independently before calling. No token is issued
/// and no filtered DTO or saved HTTP principal can stand in for this capture.
pub async fn may_curate_work_in(
    tx: &mut Transaction<'_, Sqlite>,
    host: &mut HostSession,
    work_id: &str,
    scope: &playscale_core::access::CatalogScope,
) -> anyhow::Result<bool> {
    let (_, _, memberships) = capture_work_in(tx, host, work_id).await?;
    Ok(playscale_core::access::may_curate(scope, &memberships))
}

async fn capture_work_in(
    tx: &mut Transaction<'_, Sqlite>,
    host: &mut HostSession,
    work_id: &str,
) -> anyhow::Result<(u64, CompleteWorkSnapshot, BTreeSet<String>)> {
    let generation = host.catalog_generation()?;
    let capture = host
        .capture_work_dependencies(generation, work_id)
        .map_err(|e| anyhow::anyhow!("complete work capture: {e:?}"))?;
    let facts = capture.dependencies();
    let revisions = facts["entity_revisions"]
        .as_array()
        .context("capture revisions missing")?;
    let head = revisions
        .iter()
        .find(|r| {
            r["entity_kind"] == "items" && r["entity_key"] == json!([work_id]) && r["deleted"] == 0
        })
        .and_then(|r| r["revision"].as_i64())
        .context("captured work head missing")?;
    let metadata_revision = crate::revision_bridge::wire_revision_in(
        &mut **tx,
        work_id,
        capture.catalog_id(),
        capture.incarnation(),
        head,
    )
    .await?;
    let baseline: i64 =
        sqlx::query_scalar("SELECT motion_base FROM catabolic_revision_bridges WHERE native_id=?")
            .bind(work_id)
            .fetch_one(&mut **tx)
            .await?;
    let files: BTreeMap<&str, &str> = facts["files"]
        .as_array()
        .context("captured files missing")?
        .iter()
        .map(|f| {
            Ok((
                f["id"].as_str().context("captured file ID missing")?,
                f["location"]
                    .as_str()
                    .context("captured file source missing")?,
            ))
        })
        .collect::<anyhow::Result<_>>()?;
    let locations: BTreeSet<&str> = facts["associations"]
        .as_array()
        .context("captured associations missing")?
        .iter()
        .filter(|a| a["active"] == 1)
        .map(|a| {
            files
                .get(a["file_id"].as_str().context("association file missing")?)
                .copied()
                .context("association file outside complete capture")
        })
        .collect::<anyhow::Result<_>>()?;
    let mut memberships = BTreeMap::new();
    let mut checked = BTreeMap::new();
    for library in capture.library_definitions() {
        if !library["retirement_kind"].is_null() {
            continue;
        }
        let metadata: Value = serde_json::from_str(
            library["metadata"]
                .as_str()
                .context("library metadata absent")?,
        )?;
        let definition = &metadata["motion_library"];
        let id = definition["id"]
            .as_str()
            .context("library identity absent")?;
        ensure!(
            definition["contract_version"] == 1
                && library["id"] == format!("motion.library:{id}")
                && library["kind"] == "custom:motion_library",
            "unsupported native library definition"
        );
        let native_head = library["native_item_revision"]
            .as_i64()
            .context("library native head absent")?;
        let revision = crate::revision_bridge::wire_revision_in(
            &mut **tx,
            library["id"].as_str().unwrap(),
            capture.catalog_id(),
            capture.incarnation(),
            native_head,
        )
        .await?;
        let sources: Vec<String> = serde_json::from_value(definition["source_ids"].clone())?;
        let expected: BTreeSet<_> = sources.iter().cloned().collect();
        ensure!(
            sources.len() <= 100 && sources.len() == expected.len(),
            "invalid complete library source set"
        );
        type Scope = (String, String, String, String, String, i64, String);
        let scopes:Vec<Scope>=sqlx::query_as("SELECT catalog_id,incarnation,source_digest,source_mappings_json,query_id,query_revision,query_digest FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=?")
            .bind(capture.profile()).bind(id).bind(i64::try_from(revision)?).fetch_all(&mut **tx).await?;
        ensure!(
            scopes.len() == 1,
            "library lacks unique current fixed-scope proof"
        );
        let (catalog, incarnation, digest, raw, query, query_revision, query_digest) =
            scopes.into_iter().next().unwrap();
        ensure!(
            catalog == capture.catalog_id()
                && incarnation == capture.incarnation()
                && catabolic_core::sha256(raw.as_bytes()) == digest,
            "library scope identity/digest changed"
        );
        let mappings: Vec<crate::sources::SourceMapping> = serde_json::from_str(&raw)?;
        ensure!(
            mappings.len() == expected.len()
                && mappings
                    .iter()
                    .map(|m| m.library_id.clone())
                    .collect::<BTreeSet<_>>()
                    == expected,
            "library definition lacks exact source proof"
        );
        let reference = SavedQueryRef::new(query, query_revision, query_digest)
            .map_err(|e| anyhow::anyhow!("scope reference: {e:?}"))?;
        host.validate_selection_reference(&reference)?;
        let mut member = false;
        for mapping in mappings {
            ensure!(
                mapping.catalog.catalog_id == catalog
                    && mapping.catalog.incarnation == incarnation
                    && mapping.profile == capture.profile(),
                "library mapping identity mismatch"
            );
            if let Some(previous) = checked.get(&mapping.library_id) {
                ensure!(previous == &mapping, "conflicting complete source proof");
            } else {
                crate::sources::check_mapping_in(&mut **tx, &mapping).await?;
                ensure!(
                    host.source_binding(&mapping.native_source)? == mapping.native_binding
                        && host.source_observation_policy(&mapping.native_source)?
                            == mapping.native_policy,
                    "native source proof changed during capture"
                );
                checked.insert(mapping.library_id.clone(), mapping.clone());
                ensure!(checked.len() <= 1000, "complete source proof exceeds bound");
            }
            member |= locations.contains(mapping.native_source.as_str());
        }
        if member {
            memberships.insert(id.to_owned(),json!({"definition":definition,"native_revision":native_head,"wire_revision":revision}));
        }
    }
    let after = host.identity()?;
    ensure!(
        host.catalog_generation()? == generation
            && after["catalog_id"] == capture.catalog_id()
            && after["catalog_incarnation"] == capture.incarnation(),
        "native capture changed before issuance"
    );
    let complete = json!({"native":facts,"fixed_library_memberships":&memberships});
    let bytes = serde_json::to_vec(&complete)?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "complete work/library dependency budget exceeded"
    );
    let snapshot = CompleteWorkSnapshot {
        catalog_id: capture.catalog_id().into(),
        incarnation: capture.incarnation().into(),
        work_id: work_id.into(),
        native_generation: generation,
        observation_epoch: 0,
        metadata_revision,
        dependencies: vec![Dependency {
            kind: DependencyKind::Work,
            id: work_id.into(),
            native_revision: Some(head.try_into()?),
            content_digest: catabolic_core::sha256(&bytes),
        }],
    };
    Ok((
        baseline.try_into()?,
        snapshot,
        memberships.into_keys().collect(),
    ))
}
