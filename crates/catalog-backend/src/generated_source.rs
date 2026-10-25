//! Private managed-output root facts, never public source/library membership.
use crate::sources::SourceMapping;
use anyhow::{Context, ensure};
use rusqlite::OptionalExtension;

pub const SOURCE_ID: &str = "motion:generated-output";

pub fn validate(mapping: &SourceMapping) -> anyhow::Result<()> {
    ensure!(
        mapping.library_id == SOURCE_ID
            && mapping.native_source == catabolic_core::sha256(SOURCE_ID.as_bytes())
            && mapping.motion_binding_revision == 1
            && mapping.exclusions.is_empty(),
        "invalid private generated source identity"
    );
    ensure!(
        mapping.native_policy["exclusions"] == serde_json::json!([])
            && mapping.native_policy["extensions"].is_null()
            && mapping.native_policy["include_regex"] == serde_json::json!([])
            && mapping.native_policy["exclude_regex"] == serde_json::json!([]),
        "private generated observation policy is not complete"
    );
    ensure!(
        mapping.native_binding["root"].as_str() == Some(&mapping.motion_root),
        "private generated native root differs from host root"
    );
    let device = mapping.native_binding["device"]
        .as_u64()
        .context("generated root device missing")?;
    let inode = mapping.native_binding["inode"]
        .as_u64()
        .context("generated root inode missing")?;
    ensure!(
        mapping.motion_root_identity == format!("{device}:{inode}"),
        "private generated root identity differs"
    );
    Ok(())
}
fn decode(raw: String) -> anyhow::Result<SourceMapping> {
    let mapping = serde_json::from_str(&raw)?;
    validate(&mapping)?;
    Ok(mapping)
}
pub async fn mapping_in(db: &mut sqlx::SqliteConnection) -> anyhow::Result<Option<SourceMapping>> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='catabolic_generated_source')")
        .fetch_one(&mut *db).await?;
    if !exists {
        return Ok(None);
    }
    let raw: Option<String> =
        sqlx::query_scalar("SELECT mapping_json FROM catabolic_generated_source WHERE id=?")
            .bind(SOURCE_ID)
            .fetch_optional(&mut *db)
            .await?;
    raw.map(decode).transpose()
}
pub fn mapping_sync(db: &rusqlite::Connection) -> anyhow::Result<Option<SourceMapping>> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='catabolic_generated_source')", [], |row| row.get(0))?;
    if !exists {
        return Ok(None);
    }
    let raw: Option<String> = db
        .query_row(
            "SELECT mapping_json FROM catabolic_generated_source WHERE id=?",
            [SOURCE_ID],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(decode).transpose()
}
/// An immutable host row is not sufficient authority: recheck the actual retained
/// native owner, exact filesystem binding and current observation policy.
pub fn verify_with_host(
    host: &catabolic_runtime::host::HostSession,
    mapping: &SourceMapping,
) -> anyhow::Result<()> {
    validate(mapping)?;
    let identity = host.identity()?;
    ensure!(
        identity["catalog_id"].as_str() == Some(&mapping.catalog.catalog_id)
            && identity["catalog_incarnation"].as_str() == Some(&mapping.catalog.incarnation)
            && identity["profile"].as_str() == Some(&mapping.profile),
        "private generated catalog identity changed"
    );
    let current = playscale_core::catalog_backend::Identity {
        catalog_id: mapping.catalog.catalog_id.clone(),
        incarnation: mapping.catalog.incarnation.clone(),
        schema_version: identity["schema_version"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .context("private generated schema missing")?,
    };
    playscale_core::catalog_backend::admit_retained_source_record(&current, &mapping.catalog)
        .map_err(|e| anyhow::anyhow!("private generated source envelope: {e:?}"))?;
    let binding = host.source_binding(&mapping.native_source)?;
    ensure!(
        binding == mapping.native_binding
            && host.source_observation_policy(&mapping.native_source)? == mapping.native_policy,
        "private generated native binding or policy changed"
    );
    crate::sources::check_root(&binding)?;
    Ok(())
}

/// Disposable FK row only. These fixed fields do not confer public source or
/// library membership, permissions, scan scheduling, or processing authority.
pub fn descriptor_row(mapping: &SourceMapping) -> anyhow::Result<serde_json::Value> {
    validate(mapping)?;
    Ok(
        serde_json::json!({"id":SOURCE_ID,"name":"Generated media","root":mapping.motion_root,
        "root_identity":mapping.motion_root_identity,"managed":1,"enabled":1,"revision":0,
        "binding_revision":mapping.motion_binding_revision,"exclusions_json":"[]","scan_barrier":0}),
    )
}
pub fn validate_cached_row(
    mapping: &SourceMapping,
    cached: &serde_json::Value,
) -> anyhow::Result<()> {
    ensure!(
        *cached == descriptor_row(mapping)?,
        "private generated descriptor cache differs from exact owner proof"
    );
    Ok(())
}
