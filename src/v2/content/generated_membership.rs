//! Receipt-backed inherited membership, separate from output byte ownership.
use super::Problem;
use anyhow::{Context, ensure};
use motion_catalog_backend::{
    generated_publication,
    occurrence_bridge::OccurrenceMapping,
    source_lifecycle::{self, LifecycleProof, State},
};
use serde::Deserialize;
use serde_json::Value;
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
struct Publication {
    operation_id: String,
    payload_digest: String,
    receipt_digest: String,
    admission_digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    contract_version: i64,
    kind: String,
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
    input_source_id: String,
    input_native_source: String,
    input_source_mapping_digest: String,
    input_occurrence_mapping_digest: String,
    output_occurrence_mapping_digest: String,
    publication: Publication,
    lifecycle: LifecycleProof,
}
fn hash(value: &impl serde::Serialize) -> anyhow::Result<String> {
    Ok(catabolic_core::sha256(serde_json::to_vec(
        &serde_json::to_value(value)?,
    )?))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Called only after output receipt origin and current selected readiness were
/// proved under the same native application writer. Absence grants no scope.
pub(super) async fn libraries(
    conn: &mut sqlx::SqliteConnection,
    file: &str,
    version: &str,
    catalog: &str,
    incarnation: &str,
    generation: i64,
    output: &OccurrenceMapping,
    granted: &BTreeSet<String>,
) -> Result<BTreeSet<String>, Problem> {
    checked(
        conn,
        file,
        version,
        catalog,
        incarnation,
        generation,
        output,
        granted,
    )
    .await
    .map_err(Problem::internal)
}
async fn checked(
    conn: &mut sqlx::SqliteConnection,
    file: &str,
    version: &str,
    catalog: &str,
    incarnation: &str,
    generation: i64,
    output: &OccurrenceMapping,
    granted: &BTreeSet<String>,
) -> anyhow::Result<BTreeSet<String>> {
    let rows:Vec<(String,String,String,String)>=sqlx::query_as("SELECT operation_id,job_id,command_json,receipt_json FROM catabolic_generated_publications WHERE receipt_json IS NOT NULL AND json_extract(receipt_json,'$.result.version_id')=? LIMIT 2").bind(version).fetch_all(&mut *conn).await?;
    ensure!(rows.len() == 1, "generated membership receipt is ambiguous");
    let (operation, job, raw_command, raw_receipt) = &rows[0];
    ensure!(
        raw_command.len() <= 4 * 1024 * 1024 && raw_receipt.len() <= 4 * 1024 * 1024,
        "generated membership receipt exceeds bound"
    );
    let saved: Value = serde_json::from_str(raw_command)?;
    let receipt: Value = serde_json::from_str(raw_receipt)?;
    let command = generated_publication::decode_command(&saved["command"])?;
    ensure!(
        command.key().operation_id() == operation
            && command.key().catalog_id() == catalog
            && command.admission_incarnation() == incarnation
            && command.payload()["request"]["job_id"] == *job,
        "generated membership command identity changed"
    );
    let raw: String = sqlx::query_scalar(
        "SELECT capture_json FROM catabolic_processing_admissions WHERE job_id=?",
    )
    .bind(job)
    .fetch_one(&mut *conn)
    .await?;
    ensure!(
        raw.len() <= 4 * 1024 * 1024,
        "generated membership admission exceeds bound"
    );
    let admission: Value = serde_json::from_str(&raw)?;
    let input = generated_publication::input_source(&command, &admission)?;
    let lifecycle = source_lifecycle::current_in(conn, &input.occurrence.source.library_id).await?;
    ensure!(
        lifecycle.catalog_id == catalog
            && lifecycle.incarnation == incarnation
            && lifecycle.mapping_digest == input.source_mapping_digest,
        "generated membership lifecycle identity changed"
    );
    // Withdrawal removes inherited scope, not independent managed output bytes.
    if lifecycle.state != State::Active {
        return Ok(BTreeSet::new());
    }
    let receipt_digest = hash(&receipt)?;
    let output_digest = hash(output)?;
    ensure!(granted.len() <= 200, "generated scope exceeds policy bound");
    let rows:Vec<(String,String)>=sqlx::query_as("SELECT library_id,proof_json FROM catabolic_projection_generated_libraries WHERE file_id=? AND generation=? AND library_id IN (SELECT value FROM json_each(?)) ORDER BY library_id").bind(file).bind(generation).bind(serde_json::to_string(granted)?).fetch_all(&mut *conn).await?;
    ensure!(
        rows.len() <= 200,
        "generated membership exceeds policy bound"
    );
    let mut found = BTreeSet::new();
    for (library, raw) in rows {
        ensure!(
            raw.len() <= 64 * 1024,
            "generated membership row exceeds bound"
        );
        let p: Proof = serde_json::from_str(&raw).context("invalid generated membership proof")?;
        ensure!(
            p.contract_version == 2
                && p.kind == "generated"
                && p.catalog_id == catalog
                && p.incarnation == incarnation
                && p.generation == generation
                && p.file_id == file
                && p.library_id == library
                && p.library_native_id == format!("motion.library:{library}")
                && p.library_native_revision > 0
                && p.library_revision >= 0
                && !p.scope.query_id.is_empty()
                && p.scope.query_revision > 0,
            "generated membership identity changed"
        );
        ensure!(
            [
                &p.definition_digest,
                &p.scope.query_digest,
                &p.scope.source_digest,
                &p.input_source_mapping_digest,
                &p.input_occurrence_mapping_digest,
                &p.output_occurrence_mapping_digest,
                &p.publication.payload_digest,
                &p.publication.receipt_digest,
                &p.publication.admission_digest
            ]
            .into_iter()
            .all(|d| digest(d)),
            "generated membership digest malformed"
        );
        ensure!(
            p.input_source_id == input.occurrence.source.library_id
                && p.input_native_source == input.occurrence.source.native_source
                && p.input_source_mapping_digest == input.source_mapping_digest
                && p.input_occurrence_mapping_digest == input.occurrence_mapping_digest
                && p.output_occurrence_mapping_digest == output_digest
                && p.publication.operation_id == *operation
                && p.publication.payload_digest == command.digest().as_str()
                && p.publication.receipt_digest == receipt_digest
                && p.publication.admission_digest == input.admission_digest
                && p.lifecycle == lifecycle,
            "generated membership provenance or lifecycle changed"
        );
        found.insert(library);
    }
    Ok(found)
}
