//! Consume only the disposable proof emitted by the native projection owner.
//! This does not create a library grant or infer one from legacy source joins.
use super::Problem;
use motion_catalog_backend::occurrence_bridge::OccurrenceMapping;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    query_id: String,
    query_revision: i64,
    query_digest: String,
    source_digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    contract_version: i64,
    catalog_id: String,
    incarnation: String,
    generation: i64,
    file_id: String,
    library_id: String,
    library_native_id: String,
    library_native_revision: i64,
    library_revision: i64,
    definition_digest: String,
    scope: Scope,
    source_id: String,
    native_source: String,
    source_mapping_digest: String,
    occurrence_mapping_digest: String,
}
fn unavailable() -> Problem {
    Problem::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "native_scope_unavailable",
        "Current native file membership proof is unavailable",
    )
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) async fn libraries(
    conn: &mut sqlx::SqliteConnection,
    file: &str,
) -> Result<BTreeSet<String>, Problem> {
    let (catalog, incarnation, generation, mapping): (String, String, i64, String) = sqlx::query_as(
        "SELECT s.catalog_id,s.incarnation,s.generation,f.mapping_json FROM catabolic_projection_state s JOIN catabolic_projection_files f ON f.generation=s.generation WHERE s.singleton=1 AND f.file_id=? AND f.available=1",
    ).bind(file).fetch_optional(&mut *conn).await?.ok_or_else(unavailable)?;
    let mapping: OccurrenceMapping = serde_json::from_str(&mapping).map_err(|_| unavailable())?;
    let occurrence_digest = catabolic_core::sha256(
        serde_json::to_vec(&serde_json::to_value(&mapping).map_err(|_| unavailable())?)
            .map_err(|_| unavailable())?,
    );
    let source_digest = catabolic_core::sha256(
        serde_json::to_vec(&serde_json::to_value(&mapping.source).map_err(|_| unavailable())?)
            .map_err(|_| unavailable())?,
    );
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT library_id,proof_json FROM catabolic_projection_file_libraries WHERE file_id=? AND generation=? ORDER BY library_id",
    ).bind(file).bind(generation).fetch_all(conn).await?;
    let mut libraries = BTreeSet::new();
    for (library, raw) in rows {
        let proof: Proof = serde_json::from_str(&raw).map_err(|_| unavailable())?;
        if proof.contract_version != 1
            || proof.catalog_id != catalog
            || proof.incarnation != incarnation
            || proof.generation != generation
            || proof.file_id != file
            || proof.library_id != library
            || proof.library_native_id != format!("motion.library:{library}")
            || proof.library_native_revision < 1
            || proof.library_revision < 0
            || proof.scope.query_id.is_empty()
            || proof.scope.query_revision < 1
            || ![
                &proof.definition_digest,
                &proof.scope.query_digest,
                &proof.scope.source_digest,
                &proof.source_mapping_digest,
                &proof.occurrence_mapping_digest,
            ]
            .into_iter()
            .all(|d| digest(d))
            || proof.source_id != mapping.source.library_id
            || proof.native_source != mapping.source.native_source
            || proof.source_mapping_digest != source_digest
            || proof.occurrence_mapping_digest != occurrence_digest
        {
            return Err(unavailable());
        }
        libraries.insert(library);
    }
    Ok(libraries)
}
