//! Exact occurrence identity translation. A path is only a candidate selector;
//! the frozen Motion fingerprint must equal the native observation's full tuple.
use crate::{NativeCatalog, sources::SourceMapping};
use anyhow::{Context, ensure};
use catabolic_store::catalog_import_typed::{
    ImportedAssociation, NativeIdentityBatch, PhysicalMapping, TypedAuthority,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OccurrenceMapping {
    pub source: SourceMapping,
    pub native_generation: i64,
    pub physical: PhysicalMapping,
    pub motion_file: Value,
    pub version_files: Vec<Value>,
    pub association: ImportedAssociation,
}
#[derive(Default)]
pub struct CapturedOccurrences {
    pub mappings: Vec<OccurrenceMapping>,
    pub unresolved: BTreeMap<String, String>,
}
impl OccurrenceMapping {
    pub fn typed(&self) -> anyhow::Result<NativeIdentityBatch> {
        let mut associations = Vec::new();
        for version in &self.version_files {
            let id = text(version, "version_id")?;
            let part = version["part"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
                .context("invalid version file part")?;
            ensure!(
                version["file_revision"] == self.motion_file["revision"],
                "version binding refers to a different historical Motion content revision"
            );
            associations.push(ImportedAssociation {
                source_association_id: format!("motion.version-file:{id}:{part}"),
                source_occurrence_id: self.physical.source_occurrence_id.clone(),
                item_id: format!("motion.version:{id}"),
                part,
            });
        }
        Ok(NativeIdentityBatch {
            occurrences: vec![self.physical.clone()],
            associations,
            mutations: vec![],
        })
    }
}
fn text<'a>(v: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    v[key].as_str().with_context(|| format!("missing {key}"))
}
/// Equality of independently captured identity facts, not content-hash matching.
fn matches_fingerprint(motion: &Value, observed: &Value) -> bool {
    use playscale_core::catalog_registration::{ObservedOccurrenceIdentity, admits_occurrence};
    let parsed = (|| -> Option<_> {
        Some((
            motion["fingerprint"].as_str()?,
            motion["bytes"].as_i64()?,
            ObservedOccurrenceIdentity {
                size: observed["size"].as_i64()?,
                mtime_ns: observed["mtime_ns"].as_i64()?,
                device: observed["device"].as_i64()?,
                inode: observed["inode"].as_i64()?,
                ctime_ns: observed["ctime_ns"].as_i64()?,
            },
        ))
    })();
    parsed.is_some_and(|(fingerprint, bytes, identity)| {
        admits_occurrence(
            fingerprint,
            bytes,
            motion["available"].as_i64() == Some(1),
            observed["status"] == "present",
            identity,
        )
    })
}

impl NativeCatalog {
    /// Called only while the trusted exporter retains its frozen Motion writer
    /// transaction. Replay reads immutable mappings, never recaptures live files.
    pub async fn capture_motion_occurrences(
        &self,
        db: &mut SqliteConnection,
        import_id: &str,
    ) -> anyhow::Result<CapturedOccurrences> {
        let files = crate::motion_export::rows(db, "media_files", None).await?;
        let versions = crate::motion_export::rows(db, "version_files", None).await?;
        let mut result = CapturedOccurrences::default();
        let mut snapshots = BTreeMap::<String, (SourceMapping, Value)>::new();
        let mut parts = BTreeMap::<String, u32>::new();
        for file in files {
            let id = text(&file, "id")?.to_owned();
            let old:Option<String>=sqlx::query_scalar("SELECT mapping_json FROM catabolic_occurrence_mappings WHERE import_id=? AND motion_file_id=?").bind(import_id).bind(&id).fetch_optional(&mut *db).await?;
            if let Some(old) = old {
                let mapped: OccurrenceMapping = serde_json::from_str(&old)?;
                ensure!(
                    mapped.motion_file == file,
                    "frozen occurrence changed on replay"
                );
                result.mappings.push(mapped);
                continue;
            }
            let library = text(&file, "library_id")?.to_owned();
            if !snapshots.contains_key(&library) {
                let raw: Option<String> = sqlx::query_scalar(
                    "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
                )
                .bind(&library)
                .fetch_optional(&mut *db)
                .await?;
                let Some(raw) = raw else {
                    result
                        .unresolved
                        .insert(id, "source is not independently registered".into());
                    continue;
                };
                let mapping: SourceMapping = serde_json::from_str(&raw)?;
                let (root, root_identity, revision, exclusions, managed):(String,String,i64,String,i64) = sqlx::query_as("SELECT root,root_identity,binding_revision,exclusions_json,managed FROM libraries WHERE id=?").bind(&library).fetch_one(&mut *db).await?;
                let normalized =
                    playscale_core::sources::normalize_exclusions(&serde_json::from_str::<
                        Vec<String>,
                    >(
                        &exclusions
                    )?)
                    .map_err(|e| anyhow::anyhow!("source exclusions: {e:?}"))?;
                ensure!(
                    root == mapping.motion_root
                        && root_identity == mapping.motion_root_identity
                        && revision == mapping.motion_binding_revision
                        && managed == 0
                        && normalized == mapping.exclusions,
                    "frozen Motion source no longer matches immutable native binding"
                );
                let check = mapping.clone();
                let snapshot = self
                    .with_store(true, move |store| {
                        crate::sources::inspect_mapping(store, &check)?;
                        Ok(
                            catabolic_store::catalog_import_typed::capture_source_occurrences(
                                store,
                                &check.profile,
                                &check.native_source,
                            )?,
                        )
                    })
                    .await?;
                snapshots.insert(library.clone(), (mapping, snapshot));
            }
            let (source, snapshot) = &snapshots[&library];
            let path = text(&file, "relative_path")?;
            let observed = snapshot["occurrences"]
                .as_array()
                .context("native occurrences missing")?
                .iter()
                .find(|o| o["path"].as_str() == Some(path));
            let Some(observed) = observed.filter(|o| matches_fingerprint(&file, o)) else {
                result.unresolved.insert(
                    id,
                    "native exact observation does not match frozen Motion identity".into(),
                );
                continue;
            };
            let edition = text(&file, "edition_id")?.to_owned();
            let part = parts.entry(edition.clone()).or_default();
            *part = part
                .checked_add(1)
                .context("too many edition occurrences")?;
            let physical = PhysicalMapping {
                source_occurrence_id: id.clone(),
                profile: source.profile.clone(),
                location: source.native_source.clone(),
                path: path.into(),
                exact: catabolic_core::catalog_identity::ExactFileRevision {
                    file_id: text(observed, "id")?.into(),
                    revision: text(observed, "exact_revision")?.into(),
                },
                observed_status: Some("present".into()),
            };
            let mapping = OccurrenceMapping {
                source: source.clone(),
                native_generation: snapshot["generation"]
                    .as_i64()
                    .context("missing native generation")?,
                physical,
                motion_file: file,
                version_files: versions
                    .iter()
                    .filter(|v| v["file_id"].as_str() == Some(&id))
                    .cloned()
                    .collect(),
                association: ImportedAssociation {
                    source_association_id: format!("motion.file:{id}"),
                    source_occurrence_id: id.clone(),
                    item_id: format!("motion.edition:{edition}"),
                    part: *part,
                },
            };
            sqlx::query("INSERT INTO catabolic_occurrence_mappings(import_id,motion_file_id,native_profile,native_file_id,mapping_json) VALUES(?,?,?,?,?)").bind(import_id).bind(&id).bind(&mapping.physical.profile).bind(&mapping.physical.exact.file_id).bind(serde_json::to_string(&mapping)?).execute(&mut *db).await?;
            result.mappings.push(mapping);
        }
        Ok(result)
    }
    /// Trusted export provenance supplies import_id. This API never grants a
    /// caller-supplied native file ID and never infers authority from path/digest.
    pub async fn occurrence_authority(&self, import_id: &str) -> anyhow::Result<TypedAuthority> {
        let rows:Vec<String>=sqlx::query_scalar("SELECT mapping_json FROM catabolic_occurrence_mappings WHERE import_id=? ORDER BY motion_file_id").bind(import_id).fetch_all(&self.journal).await?;
        let mut grant = TypedAuthority::default();
        for row in rows {
            let mapping: OccurrenceMapping = serde_json::from_str(&row)?;
            self.identity
                .admit(&mapping.source.catalog)
                .map_err(|e| anyhow::anyhow!("occurrence catalog: {e:?}"))?;
            grant
                .physical_files
                .entry(mapping.physical.profile)
                .or_insert_with(BTreeSet::new)
                .insert(mapping.physical.exact.file_id);
        }
        Ok(grant)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_never_follows_only_path_or_size() {
        let m = json!({"fingerprint":"10:20:30:40:50:60","bytes":10,"available":1});
        let o = json!({"size":10,"mtime_ns":20,"device":30,"inode":40,"ctime_ns":50000000060_i64,"status":"present"});
        assert!(matches_fingerprint(&m, &o));
        for key in ["size", "mtime_ns", "device", "inode", "ctime_ns"] {
            let mut bad = o.clone();
            bad[key] = json!(0);
            assert!(!matches_fingerprint(&m, &bad));
        }
        let mut missing = o;
        missing["status"] = json!("missing");
        assert!(!matches_fingerprint(&m, &missing));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OccurrenceProjection {
    pub catalog: playscale_core::catalog_backend::Identity,
    pub generation: i64,
    pub sources: Vec<SourceMapping>,
    pub files: Vec<Value>,
    /// Exact native missing/replaced observation, never absence from a partial inventory.
    pub retired_file_ids: BTreeSet<String>,
    pub version_files: Vec<Value>,
    /// Native discoveries without an independently established Motion identity
    /// remain visible as discoveries, never guessed into playback references.
    pub discoveries: Vec<Value>,
}
/// Rebuild disposable playback projections from the actual retained owner.
/// Changed/missing content invalidates availability; old content revisions and
/// version bindings are never silently updated to newly observed bytes.
pub fn project_with_host(
    host: &catabolic_runtime::host::HostSession,
    mappings: &[OccurrenceMapping],
) -> anyhow::Result<OccurrenceProjection> {
    let identity = host.identity()?;
    let catalog = playscale_core::catalog_backend::Identity {
        catalog_id: text(&identity, "catalog_id")?.into(),
        incarnation: text(&identity, "catalog_incarnation")?.into(),
        schema_version: identity["schema_version"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .context("invalid host schema")?,
    };
    let mut snapshots = BTreeMap::<String, Value>::new();
    let mut generation = None;
    let mut sources = Vec::new();
    for mapping in mappings {
        playscale_core::catalog_backend::admit_retained_source_record(
            &catalog,
            &mapping.source.catalog,
        )
        .map_err(|e| anyhow::anyhow!("occurrence projection identity: {e:?}"))?;
        ensure!(
            identity["profile"].as_str() == Some(&mapping.source.profile),
            "occurrence profile mismatch"
        );
        if !snapshots.contains_key(&mapping.source.native_source) {
            let snapshot = host.source_occurrences(&mapping.source.native_source)?;
            ensure!(
                snapshot["binding"] == mapping.source.native_binding
                    && snapshot["policy"] == mapping.source.native_policy,
                "occurrence source changed"
            );
            crate::sources::check_root(&snapshot["binding"])?;
            let current = snapshot["generation"]
                .as_i64()
                .context("missing generation")?;
            ensure!(
                generation.is_none_or(|g| g == current),
                "native generation changed during projection; retry whole read"
            );
            generation = Some(current);
            sources.push(mapping.source.clone());
            snapshots.insert(mapping.source.native_source.clone(), snapshot);
        }
    }
    let mut files = Vec::new();
    let mut retired_file_ids = BTreeSet::new();
    let mut versions = Vec::new();
    let mut known = BTreeSet::new();
    for mapping in mappings {
        let observed = snapshots[&mapping.source.native_source]["occurrences"]
            .as_array()
            .context("missing occurrence list")?
            .iter()
            .find(|o| o["id"].as_str() == Some(&mapping.physical.exact.file_id));
        let present = observed.is_some_and(|o| {
            o["exact_revision"].as_str() == Some(&mapping.physical.exact.revision)
                && matches_fingerprint(&mapping.motion_file, o)
        });
        use playscale_core::catalog_registration::{OccurrencePresence, retires_occurrence};
        let retired = observed.is_some_and(|o| {
            let presence = match o["status"].as_str() {
                Some("present") => OccurrencePresence::Present,
                Some("missing") => OccurrencePresence::Missing,
                _ => OccurrencePresence::Unknown,
            };
            retires_occurrence(
                presence,
                &mapping.physical.exact.revision,
                o["exact_revision"].as_str(),
                ["size", "mtime_ns", "device", "inode", "ctime_ns"]
                    .iter()
                    .all(|key| o[*key].as_i64().is_some())
                    .then(|| matches_fingerprint(&mapping.motion_file, o)),
            )
        });
        if !present && retired {
            retired_file_ids.insert(text(&mapping.motion_file, "id")?.to_owned());
        }
        let mut file = mapping.motion_file.clone();
        file["available"] = json!(i64::from(present));
        files.push(file);
        versions.extend(mapping.version_files.clone());
        known.insert(mapping.physical.exact.file_id.clone());
    }
    let discoveries = snapshots
        .values()
        .flat_map(|s| s["occurrences"].as_array().into_iter().flatten())
        .filter(|o| o["id"].as_str().is_some_and(|id| !known.contains(id)))
        .cloned()
        .collect();
    Ok(OccurrenceProjection {
        catalog,
        generation: generation
            .context("occurrence projection requires at least one registered mapping")?,
        sources,
        files,
        retired_file_ids,
        version_files: versions,
        discoveries,
    })
}
impl NativeCatalog {
    pub async fn occurrence_mappings(
        &self,
        import_id: &str,
    ) -> anyhow::Result<Vec<OccurrenceMapping>> {
        let rows:Vec<String>=sqlx::query_scalar("SELECT mapping_json FROM catabolic_occurrence_mappings WHERE import_id=? ORDER BY motion_file_id").bind(import_id).fetch_all(&self.journal).await?;
        rows.into_iter()
            .map(|r| Ok(serde_json::from_str(&r)?))
            .collect()
    }
}
