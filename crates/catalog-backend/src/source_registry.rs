//! Native-owned source registration independent of frozen legacy libraries.
use crate::{NativeCatalog, sources::SourceMapping};
use anyhow::{Context, ensure};
use catabolic_store::observation;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSourceDescriptor {
    pub id: String,
    pub logical_library_id: String,
    pub name: String,
    pub root: String,
    pub root_identity: String,
    pub exclusions: Vec<String>,
}
impl NativeCatalog {
    /// IDs are explicit host-assigned identities, never guessed from a path.
    /// Native bind commits before the immutable host acknowledgment. A retry
    /// adopts only the exact catalog/root/policy, never replaces a binding.
    pub async fn register_native_source(
        &self,
        id: &str,
        logical_library_id: &str,
        name: &str,
        root: PathBuf,
        exclusions: Vec<String>,
    ) -> anyhow::Result<SourceMapping> {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            !id.is_empty()
                && id.len() <= 256
                && !logical_library_id.is_empty()
                && logical_library_id.len() <= 256
                && !name.is_empty()
                && name.len() <= 1024,
            "invalid native source descriptor"
        );
        let root = root.canonicalize()?;
        let metadata = std::fs::metadata(&root)?;
        ensure!(metadata.is_dir(), "source root must be a directory");
        let root = root
            .to_str()
            .context("source root must be UTF-8")?
            .to_owned();
        let exclusions = playscale_core::sources::normalize_exclusions(&exclusions)
            .map_err(|e| anyhow::anyhow!("source exclusions: {e:?}"))?;
        let descriptor = NativeSourceDescriptor {
            id: id.into(),
            logical_library_id: logical_library_id.into(),
            name: name.into(),
            root: root.clone(),
            root_identity: format!("{}:{}", metadata.dev(), metadata.ino()),
            exclusions: exclusions.clone(),
        };
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        ensure_public_source_root_in(&mut tx, id, std::path::Path::new(&root)).await?;
        let legacy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM libraries WHERE id=?)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let saved: Option<String> =
            sqlx::query_scalar("SELECT descriptor_json FROM catabolic_source_registry WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(saved) = saved {
            ensure!(
                serde_json::from_str::<NativeSourceDescriptor>(&saved)? == descriptor,
                "native source registration identity changed"
            );
            let raw: String = sqlx::query_scalar(
                "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            let mapping: SourceMapping = serde_json::from_str(&raw)?;
            if legacy {
                verify_projected_descriptor(&mut tx, &descriptor, &mapping).await?;
            }
            let check = mapping.clone();
            self.with_store(false, move |store| {
                crate::sources::inspect_mapping(store, &check)
            })
            .await?;
            tx.rollback().await?;
            return Ok(mapping);
        }
        ensure!(
            !legacy,
            "native source ID belongs to an existing legacy source"
        );
        let source = catabolic_core::sha256(id.as_bytes());
        let source_for_bind = source.clone();
        let root_for_bind = root.clone();
        let (binding,policy)=self.with_store(true,move|store|Ok(store.transaction(|_| {
            let exists=store.db.prepare("SELECT 1 FROM bindings WHERE profile='default' AND kind='source' AND owner=?")?.exists([&source_for_bind])?;
            let binding=if exists {
                let b=observation::binding(store,"default","source",&source_for_bind)?;
                if b["root"].as_str()!=Some(&root_for_bind) {return Err(catabolic_store::Error("native source root conflict".into()));}
                crate::sources::check_root(&b).map_err(|e|catabolic_store::Error(e.to_string()))?; b
            } else {observation::execute(store,"default","bind",&json!({"kind":"source","owner":source_for_bind,"root":root_for_bind}))?};
            let before=observation::execute(store,"default","scan_policy",&json!({"source":source_for_bind}))?;
            let compatible=before["exclusions"]==json!(exclusions)&&before["extensions"].is_null()&&before["include_regex"]==json!([])&&before["exclude_regex"]==json!([]);
            if !compatible {
                if before["revision"].as_i64()!=Some(0) {return Err(catabolic_store::Error("native source policy conflict".into()));}
                observation::execute(store,"default","scan_policy",&json!({"source":source_for_bind,"apply":true,"policy":{"exclusions":exclusions,"extensions":null,"include_regex":[],"exclude_regex":[]}}))?;
            }
            let policy=observation::execute(store,"default","scan_policy",&json!({"source":source_for_bind}))?;
            Ok((binding,policy))
        })?)).await?;
        ensure!(
            binding["device"].as_u64() == Some(metadata.dev())
                && binding["inode"].as_u64() == Some(metadata.ino()),
            "native root changed during registration"
        );
        let mapping = SourceMapping {
            library_id: id.into(),
            profile: "default".into(),
            native_source: source,
            catalog: self.identity.clone(),
            motion_root: root,
            motion_root_identity: descriptor.root_identity.clone(),
            motion_binding_revision: 1,
            native_binding: binding,
            exclusions: descriptor.exclusions.clone(),
            native_policy: policy,
        };
        sqlx::query(
            "INSERT INTO catabolic_source_registry(id,descriptor_json,created_at) VALUES(?,?,?)",
        )
        .bind(id)
        .bind(serde_json::to_string(&descriptor)?)
        .bind(crate::now())
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO catabolic_source_mappings(library_id,profile,native_source,mapping_json,created_at) VALUES(?,?,?,?,?)").bind(id).bind(&mapping.profile).bind(&mapping.native_source).bind(serde_json::to_string(&mapping)?).bind(crate::now()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(mapping)
    }
    pub async fn native_sources(&self) -> anyhow::Result<Vec<NativeSourceDescriptor>> {
        let values: Vec<String> =
            sqlx::query_scalar("SELECT descriptor_json FROM catabolic_source_registry ORDER BY id")
                .fetch_all(&self.journal)
                .await?;
        values
            .into_iter()
            .map(|s| Ok(serde_json::from_str(&s)?))
            .collect()
    }
}

/// Typed proof produced only by the package-owned retained-host registration
/// path. It is not deserializable from an operator-supplied file.
pub struct PreparedNativeSource {
    descriptor: NativeSourceDescriptor,
    mapping: SourceMapping,
}
impl PreparedNativeSource {
    pub fn descriptor(&self) -> &NativeSourceDescriptor {
        &self.descriptor
    }
    pub fn mapping(&self) -> &SourceMapping {
        &self.mapping
    }
}
pub fn register_with_host(
    host: &mut catabolic_runtime::host::HostSession,
    id: &str,
    logical_library_id: &str,
    name: &str,
    root: PathBuf,
    exclusions: Vec<String>,
) -> anyhow::Result<PreparedNativeSource> {
    use std::os::unix::fs::MetadataExt;
    ensure!(
        !id.is_empty()
            && id.len() <= 256
            && !logical_library_id.is_empty()
            && logical_library_id.len() <= 256
            && !name.is_empty()
            && name.len() <= 1024,
        "invalid native source descriptor"
    );
    let root = root.canonicalize()?;
    let meta = std::fs::metadata(&root)?;
    ensure!(meta.is_dir(), "source root must be a directory");
    let root_text = root
        .to_str()
        .context("source root must be UTF-8")?
        .to_owned();
    let exclusions = playscale_core::sources::normalize_exclusions(&exclusions)
        .map_err(|e| anyhow::anyhow!("source exclusions: {e:?}"))?;
    let identity = host.identity()?;
    let profile = identity["profile"]
        .as_str()
        .context("host profile missing")?
        .to_owned();
    let catalog = playscale_core::catalog_backend::Identity {
        catalog_id: identity["catalog_id"]
            .as_str()
            .context("host catalog missing")?
            .into(),
        incarnation: identity["catalog_incarnation"]
            .as_str()
            .context("host incarnation missing")?
            .into(),
        schema_version: identity["schema_version"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .context("host schema missing")?,
    };
    let source = catabolic_core::sha256(id.as_bytes());
    let native = host.register_source(&source, &root, &exclusions)?;
    ensure!(
        native["binding"]["device"].as_u64() == Some(meta.dev())
            && native["binding"]["inode"].as_u64() == Some(meta.ino()),
        "native root changed during registration"
    );
    let descriptor = NativeSourceDescriptor {
        id: id.into(),
        logical_library_id: logical_library_id.into(),
        name: name.into(),
        root: root_text.clone(),
        root_identity: format!("{}:{}", meta.dev(), meta.ino()),
        exclusions: exclusions.clone(),
    };
    let mapping = SourceMapping {
        library_id: id.into(),
        profile,
        native_source: source,
        catalog,
        motion_root: root_text,
        motion_root_identity: descriptor.root_identity.clone(),
        motion_binding_revision: 1,
        native_binding: native["binding"].clone(),
        exclusions,
        native_policy: native["policy"].clone(),
    };
    Ok(PreparedNativeSource {
        descriptor,
        mapping,
    })
}
impl NativeCatalog {
    /// Standalone operator wrapper. Server handlers use acknowledge_native_source_in
    /// with their already retained authorization transaction.
    pub async fn acknowledge_native_source(
        &self,
        prepared: PreparedNativeSource,
    ) -> anyhow::Result<SourceMapping> {
        self.identity
            .admit(&prepared.mapping.catalog)
            .map_err(|e| anyhow::anyhow!("native registration catalog: {e:?}"))?;
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let mapping = acknowledge_native_source_in(&mut tx, prepared).await?;
        tx.commit().await?;
        Ok(mapping)
    }
}
/// Acknowledge through the caller's retained Motion authorization transaction.
/// Never opens another native owner or another Motion writer connection.
pub async fn acknowledge_native_source_in(
    db: &mut sqlx::SqliteConnection,
    prepared: PreparedNativeSource,
) -> anyhow::Result<SourceMapping> {
    let PreparedNativeSource {
        descriptor,
        mapping,
    } = prepared;
    ensure_public_source_root_in(db, &descriptor.id, std::path::Path::new(&descriptor.root))
        .await?;
    let bound: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&mut *db)
            .await?;
    let identity: playscale_core::catalog_backend::Identity = serde_json::from_str(&bound)?;
    let identity = crate::native_upgrade::effective_identity_in(db, &identity).await?;
    identity
        .admit(&mapping.catalog)
        .map_err(|e| anyhow::anyhow!("native registration catalog: {e:?}"))?;
    let legacy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM libraries WHERE id=?)")
        .bind(&descriptor.id)
        .fetch_one(&mut *db)
        .await?;
    let existing: Option<String> =
        sqlx::query_scalar("SELECT descriptor_json FROM catabolic_source_registry WHERE id=?")
            .bind(&descriptor.id)
            .fetch_optional(&mut *db)
            .await?;
    if let Some(existing) = existing {
        ensure!(
            serde_json::from_str::<NativeSourceDescriptor>(&existing)? == descriptor,
            "source descriptor conflict"
        );
        let existing: String = sqlx::query_scalar(
            "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
        )
        .bind(&descriptor.id)
        .fetch_one(&mut *db)
        .await?;
        ensure!(
            serde_json::from_str::<SourceMapping>(&existing)? == mapping,
            "native source mapping conflict"
        );
        if legacy {
            verify_projected_descriptor(db, &descriptor, &mapping).await?;
        }
    } else {
        ensure!(!legacy, "source ID belongs to legacy projection");
        sqlx::query(
            "INSERT INTO catabolic_source_registry(id,descriptor_json,created_at) VALUES(?,?,?)",
        )
        .bind(&descriptor.id)
        .bind(serde_json::to_string(&descriptor)?)
        .bind(crate::now())
        .execute(&mut *db)
        .await?;
        sqlx::query("INSERT INTO catabolic_source_mappings(library_id,profile,native_source,mapping_json,created_at) VALUES(?,?,?,?,?)").bind(&descriptor.id).bind(&mapping.profile).bind(&mapping.native_source).bind(serde_json::to_string(&mapping)?).bind(crate::now()).execute(&mut *db).await?;
    }
    Ok(mapping)
}

/// A cached legacy-shaped row is admissible only after immutable registry and
/// mapping replay already matched. Ordinary legacy rows never establish native
/// ownership, even when their root happens to match.
async fn verify_projected_descriptor(
    db: &mut sqlx::SqliteConnection,
    descriptor: &NativeSourceDescriptor,
    mapping: &SourceMapping,
) -> anyhow::Result<()> {
    let row: (String, String, String, i64, i64, i64, i64, String, i64) = sqlx::query_as(
        "SELECT name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier FROM libraries WHERE id=?",
    ).bind(&descriptor.id).fetch_one(&mut *db).await?;
    ensure!(
        row.0 == descriptor.name
            && row.1 == descriptor.root
            && row.2 == descriptor.root_identity
            && row.3 == 0
            && row.4 == 1
            && row.5 == 0
            && row.6 == mapping.motion_binding_revision
            && serde_json::from_str::<Vec<String>>(&row.7)? == descriptor.exclusions
            && row.8 == 0,
        "cached native source descriptor differs from immutable registration"
    );
    Ok(())
}

/// Deny-only separation from the private generated owner. This never grants
/// public source authority from a managed mapping. Call before native effects
/// under the same writer used for registration, and again at its host ACK.
pub async fn ensure_public_source_root_in(
    db: &mut sqlx::SqliteConnection,
    id: &str,
    canonical_root: &std::path::Path,
) -> anyhow::Result<()> {
    ensure!(
        id != "motion:generated-output",
        "private generated source ID is reserved"
    );
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='catabolic_generated_source')")
        .fetch_one(&mut *db).await?;
    if !exists {
        return Ok(());
    }
    let private: Option<String> = sqlx::query_scalar(
        "SELECT mapping_json FROM catabolic_generated_source WHERE id='motion:generated-output'",
    )
    .fetch_optional(&mut *db)
    .await?;
    if let Some(private) = private {
        let mapping: SourceMapping = serde_json::from_str(&private)?;
        ensure!(
            mapping.library_id == "motion:generated-output",
            "invalid private generated root mapping"
        );
        let root = std::path::Path::new(&mapping.motion_root);
        ensure!(
            !canonical_root.starts_with(root) && !root.starts_with(canonical_root),
            "public source root overlaps private generated output root"
        );
    }
    Ok(())
}

#[cfg(test)]
mod private_root_tests {
    use super::*;
    #[tokio::test]
    async fn public_registration_denies_private_identity_and_overlap_without_granting_it() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let mut db = pool.acquire().await.unwrap();
        assert!(
            ensure_public_source_root_in(
                &mut db,
                "motion:generated-output",
                std::path::Path::new("/unrelated")
            )
            .await
            .is_err()
        );
        ensure_public_source_root_in(&mut db, "ordinary", std::path::Path::new("/media"))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE catabolic_generated_source(id TEXT PRIMARY KEY,mapping_json TEXT NOT NULL)").execute(&mut *db).await.unwrap();
        let private = SourceMapping {
            library_id: "motion:generated-output".into(),
            profile: "default".into(),
            native_source: "deny-only-fixture".into(),
            catalog: playscale_core::catalog_backend::Identity {
                catalog_id: "deny-only".into(),
                incarnation: "deny-only".into(),
                schema_version: 39,
            },
            motion_root: "/data/native-generated".into(),
            motion_root_identity: "0:0".into(),
            motion_binding_revision: 1,
            native_binding: serde_json::Value::Null,
            exclusions: vec![],
            native_policy: serde_json::Value::Null,
        };
        sqlx::query("INSERT INTO catabolic_generated_source VALUES ('motion:generated-output',?)")
            .bind(serde_json::to_string(&private).unwrap())
            .execute(&mut *db)
            .await
            .unwrap();
        for path in [
            "/data",
            "/data/native-generated",
            "/data/native-generated/subdir",
        ] {
            assert!(
                ensure_public_source_root_in(&mut db, "ordinary", std::path::Path::new(path))
                    .await
                    .is_err(),
                "{path}"
            );
        }
        ensure_public_source_root_in(
            &mut db,
            "ordinary",
            std::path::Path::new("/data/native-generated-other"),
        )
        .await
        .unwrap();
        // Only this deny-only helper accepts the shape fixture; no native root
        // registration, descriptor projection, or publication proof is minted.
    }
}
