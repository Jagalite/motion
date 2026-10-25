//! Library scopes are compiled only from current native library definitions,
//! immutable scope acknowledgments and independently audited native sources.
use super::Runtime;
use anyhow::{Context, ensure};
use catabolic_store::catalog_scope::{SavedQueryRef, TrustedSelection};
use motion_catalog_backend::sources::SourceMapping;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) struct LibraryReadScope {
    pub selection: TrustedSelection,
    pub definitions: TrustedSelection,
    observed_libraries: Vec<Value>,
    mappings: Vec<SourceMapping>,
}
impl Runtime {
    pub(crate) async fn prepare_library_read_scope(
        &self,
        ids: BTreeSet<String>,
    ) -> anyhow::Result<LibraryReadScope> {
        // Device policy already permits 200 library IDs. Compile the audited
        // union into one native selection instead of exceeding its 16-grant
        // bound or silently truncating a principal's allowed libraries.
        ensure!(ids.len() <= 200, "library scope exceeds policy contract");
        ensure!(
            ids.iter().all(|id| !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))),
            "invalid library scope identity"
        );
        let definitions = self
            .prepare_library_definitions(Some(ids.iter().cloned().collect()))
            .await?;
        let mut custody = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let libraries = self
            .read_libraries(&mut custody, vec![definitions.clone()], None)
            .await?;
        let mut seen = BTreeSet::new();
        let mut mappings = std::collections::BTreeMap::new();
        let mut references = Vec::new();
        for row in &libraries {
            let metadata: Value =
                serde_json::from_str(row[2].as_str().context("library metadata absent")?)?;
            let definition = &metadata["motion_library"];
            let id = definition["id"]
                .as_str()
                .context("library identity absent")?;
            ensure!(
                ids.contains(id)
                    && seen.insert(id.to_owned())
                    && definition["contract_version"] == 1
                    && row[0] == format!("motion.library:{id}")
                    && row[1] == "custom:motion_library",
                "native library scope identity mismatch"
            );
            let revision = definition["catalog_revision"]
                .as_u64()
                .and_then(|v| i64::try_from(v).ok())
                .context("library revision missing")?;
            let source_ids: Vec<String> = serde_json::from_value(definition["source_ids"].clone())?;
            let expected: BTreeSet<String> = source_ids.iter().cloned().collect();
            ensure!(
                source_ids.len() <= 100 && source_ids.len() == expected.len(),
                "invalid library source set"
            );
            type Row = (String, String, String, String, String, i64, String);
            let acknowledgments:Vec<Row>=sqlx::query_as("SELECT catalog_id,incarnation,source_digest,source_mappings_json,query_id,query_revision,query_digest FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=?")
                .bind(&self.profile).bind(id).bind(revision).fetch_all(&mut *custody).await?;
            ensure!(
                acknowledgments.len() == 1,
                "library has no unique current native scope acknowledgment"
            );
            let (catalog, incarnation, digest, raw, query, query_revision, query_digest) =
                acknowledgments.into_iter().next().unwrap();
            ensure!(
                format!("{:x}", Sha256::digest(raw.as_bytes())) == digest,
                "scope source digest changed"
            );
            let proof: Vec<SourceMapping> = serde_json::from_str(&raw)?;
            let actual: BTreeSet<String> = proof.iter().map(|m| m.library_id.clone()).collect();
            ensure!(
                actual == expected && actual.len() == proof.len(),
                "native definition and source grant differ"
            );
            for mapping in proof {
                ensure!(
                    mapping.catalog.catalog_id == catalog
                        && mapping.catalog.incarnation == incarnation
                        && mapping.profile == self.profile,
                    "scope source belongs to another catalog"
                );
                // Deployment withdrawal is a Motion control decision and need
                // not advance native generation. Check it under this writer.
                motion_catalog_backend::source_lifecycle::require_active_in(
                    &mut custody,
                    &mapping.library_id,
                )
                .await?;
                motion_catalog_backend::sources::check_mapping_in(&mut custody, &mapping).await?;
                if let Some(previous) = mappings.insert(mapping.library_id.clone(), mapping.clone())
                {
                    ensure!(
                        serde_json::to_value(previous)? == serde_json::to_value(mapping)?,
                        "conflicting source scope proofs"
                    );
                }
            }
            references.push((
                catalog,
                incarnation,
                SavedQueryRef::new(query, query_revision, query_digest)
                    .map_err(|e| anyhow::anyhow!("invalid saved scope: {e:?}"))?,
            ));
        }
        // Deleted/nonexistent allowed libraries confer no visibility. Do not
        // turn a missing allowed library into an unrestricted selection.
        let mappings: Vec<_> = mappings.into_values().collect();
        ensure!(
            mappings.len() <= 1000,
            "combined native source scope exceeds bound"
        );
        let checked = mappings.clone();
        let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
        let sources = checked
            .iter()
            .map(|m| quote(&m.native_source))
            .collect::<Vec<_>>();
        let predicate = if sources.is_empty() {
            "0".into()
        } else {
            format!("a.location IN ({})", sources.join(","))
        };
        let query = format!(
            "SELECT DISTINCT w.item_id FROM catalog_items w JOIN catalog_relationships e ON e.source_id=w.item_id AND e.kind='custom:motion_edition' AND e.active=1 JOIN catalog_relationships v ON v.source_id=e.target_id AND v.kind='custom:motion_version' AND v.active=1 JOIN catalog_item_files a ON a.item_id=v.target_id AND a.active=1 WHERE json_type(w.metadata,'$.motion_catalog')='object' AND w.item_id=json_extract(w.metadata,'$.motion_catalog.item_id') AND a.profile={} AND {predicate} ORDER BY w.item_id",
            quote(&self.profile)
        );
        ensure!(
            query.len() <= 128 * 1024,
            "compiled source scope exceeds query budget"
        );
        let name = format!(
            "{:x}",
            Sha256::digest(format!("motion-library-read-union-v1:{query}"))
        );
        let (saved,custody)=self.with_host(move|host|{
            let identity=host.identity()?;
            for (catalog,incarnation,reference) in references {
                ensure!(identity["catalog_id"]==catalog && identity["catalog_incarnation"]==incarnation,"saved scope belongs to another native owner");
                host.validate_selection_reference(&reference)?;
            }
            verify_sources(host,&checked)?;
            let saved=host.prepare_selection(&name,&serde_json::json!({"version":1,"mode":"selection","selection":{"language":"sql","query":query}}))?;
            Ok((saved,custody))
        }).await?;
        custody.commit().await?;
        let reference = SavedQueryRef::new(
            saved["id"].as_str().context("scope query ID")?.into(),
            saved["revision"].as_i64().context("scope query revision")?,
            saved["digest"]
                .as_str()
                .context("scope query digest")?
                .into(),
        )
        .map_err(|e| anyhow::anyhow!("scope query: {e:?}"))?;
        let selection = TrustedSelection::new(
            reference,
            vec![
                "title".into(),
                "motion_catalog".into(),
                "motion_artwork".into(),
            ],
        )
        .map_err(|e| anyhow::anyhow!("scope fields: {e:?}"))?;
        Ok(LibraryReadScope {
            selection,
            definitions,
            observed_libraries: libraries,
            mappings,
        })
    }
    /// Caller holds current Motion authorization writer. No query mutation or
    /// projection rebuild occurs here; a changed proof refuses this request.
    pub(crate) async fn validate_library_read_scope(
        &self,
        conn: &mut sqlx::SqliteConnection,
        scope: &LibraryReadScope,
    ) -> anyhow::Result<Vec<Value>> {
        let libraries = self
            .read_libraries(conn, vec![scope.definitions.clone()], None)
            .await?;
        ensure!(
            libraries == scope.observed_libraries,
            "native library definition changed after scope preparation"
        );
        for mapping in &scope.mappings {
            motion_catalog_backend::source_lifecycle::require_active_in(conn, &mapping.library_id)
                .await?;
            motion_catalog_backend::sources::check_mapping_in(conn, mapping).await?;
        }
        let mappings = scope.mappings.clone();
        self.with_host(move |host| verify_sources(host, &mappings))
            .await?;
        Ok(libraries)
    }
}
fn verify_sources(
    host: &catabolic_runtime::host::HostSession,
    mappings: &[SourceMapping],
) -> anyhow::Result<()> {
    let identity = host.identity()?;
    for mapping in mappings {
        ensure!(
            identity["catalog_id"] == mapping.catalog.catalog_id
                && identity["catalog_incarnation"] == mapping.catalog.incarnation
                && identity["profile"] == mapping.profile,
            "native scope owner changed"
        );
        ensure!(
            host.source_binding(&mapping.native_source)? == mapping.native_binding
                && host.source_observation_policy(&mapping.native_source)? == mapping.native_policy,
            "native scope source binding changed"
        );
    }
    Ok(())
}
