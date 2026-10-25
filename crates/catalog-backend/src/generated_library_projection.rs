//! Receipt-bound original ancestry used only to derive current library scope.
//! An offline original is not withdrawal; current output custody remains separate.
use crate::{generated_publication, occurrence_bridge::OccurrenceMapping};
use anyhow::{Context, ensure};
use rusqlite::Connection;
use serde_json::{Value, json};

pub(crate) struct Ancestry {
    pub input: generated_publication::GeneratedInputSource,
    pub publication: Value,
}
pub(crate) fn ancestry(
    db: &Connection,
    output_mapping: &OccurrenceMapping,
    remaining: &mut usize,
) -> anyhow::Result<Ancestry> {
    let file = output_mapping.motion_file["id"]
        .as_str()
        .context("generated file ID missing")?;
    let bytes: i64 = db.query_row(
        "SELECT coalesce(sum(length(CAST(g.command_json AS BLOB))+length(CAST(g.output_json AS BLOB))+length(CAST(g.receipt_json AS BLOB))+coalesce(length(CAST(a.capture_json AS BLOB)),0)),0) FROM catabolic_generated_publications g LEFT JOIN catabolic_processing_admissions a ON a.job_id=g.job_id WHERE g.receipt_json IS NOT NULL AND json_extract(g.receipt_json,'$.result.output.source_occurrence_id')=?",
        [file], |r| r.get(0))?;
    let bytes = usize::try_from(bytes).context("generated proof size overflow")?;
    ensure!(
        bytes > 0 && bytes <= *remaining,
        "generated scope cumulative proof budget exceeded"
    );
    *remaining -= bytes;
    let rows: Vec<(String,String,String,String,String)> = db.prepare(
        "SELECT operation_id,job_id,command_json,output_json,receipt_json FROM catabolic_generated_publications WHERE receipt_json IS NOT NULL AND json_extract(receipt_json,'$.result.output.source_occurrence_id')=? LIMIT 2")?
        .query_map([file],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?.collect::<Result<_,_>>()?;
    ensure!(
        rows.len() == 1,
        "generated scope has no unique publication receipt"
    );
    let (operation, job, saved, output, receipt) = &rows[0];
    ensure!(
        [saved, output, receipt]
            .iter()
            .all(|s| s.len() <= 4 * 1024 * 1024),
        "generated scope proof exceeds bound"
    );
    let saved: Value = serde_json::from_str(saved)?;
    let output: Value = serde_json::from_str(output)?;
    let receipt: Value = serde_json::from_str(receipt)?;
    let command = generated_publication::decode_command(&saved["command"])?;
    ensure!(
        saved["contract_version"] == 1
            && receipt["contract_version"] == 1
            && command.key().operation_id() == operation
            && command.payload()["request"]["job_id"] == job.as_str(),
        "generated scope command changed"
    );
    let reservation: playscale_core::processing_publication::Reservation =
        serde_json::from_value(saved["reservation"].clone())?;
    reservation.validate().map_err(anyhow::Error::msg)?;
    let key = reservation.identity();
    ensure!(
        key.operation_id == *operation
            && key.catalog_id == command.key().catalog_id()
            && key.incarnation == command.admission_incarnation()
            && key.payload_digest == command.digest().as_str(),
        "generated scope reservation changed"
    );
    // The shared attempt decoder materializes its bounded ancestry. Charge the
    // actual persisted envelopes and witnesses before it allocates any rows.
    let attempts_bytes: i64 = db.query_row(
        "SELECT coalesce(sum(length(CAST(original_envelope_json AS BLOB))+length(CAST(prior_envelope_json AS BLOB))+length(CAST(envelope_json AS BLOB))+length(CAST(stale_witness_json AS BLOB))),0) FROM (SELECT * FROM catabolic_command_attempts WHERE business_id=? ORDER BY attempt LIMIT 32)",
        [operation], |r| r.get(0))?;
    let original_bytes: i64 = db.query_row(
        "SELECT coalesce(sum(n),0) FROM (SELECT length(CAST(envelope_json AS BLOB)) n FROM catabolic_commands WHERE operation_id=? UNION ALL SELECT length(CAST(command_json AS BLOB)) n FROM catabolic_discovery_intents WHERE operation_id=? AND command_json IS NOT NULL)",
        rusqlite::params![operation,operation], |r| r.get(0))?;
    let ancestry_bytes = usize::try_from(
        attempts_bytes
            .checked_add(original_bytes)
            .context("generated attempt size overflow")?,
    )?;
    ensure!(
        ancestry_bytes <= *remaining,
        "generated attempt ancestry exceeds cumulative proof budget"
    );
    *remaining -= ancestry_bytes;
    let (_, attempt) = crate::command_attempts::latest_sync(db, &command)?;
    ensure!(
        receipt["attempt_envelope"] == serde_json::from_str::<Value>(attempt.canonical_payload())?,
        "generated scope receipt attempt changed"
    );
    let private = crate::generated_source::mapping_sync(db)?
        .context("generated scope private source missing")?;
    ensure!(
        output["mapping"] == serde_json::to_value(&private)?,
        "generated scope output source changed"
    );
    let reconstructed = generated_publication::mapping(
        &command,
        &receipt["result"],
        &private,
        &output["probe"],
        receipt["committed_generation"]
            .as_i64()
            .context("generated scope generation missing")?,
    )?;
    ensure!(
        reconstructed == *output_mapping,
        "generated scope output occurrence changed"
    );
    let raw: String = db.query_row(
        "SELECT capture_json FROM catabolic_processing_admissions WHERE job_id=?",
        [job],
        |r| r.get(0),
    )?;
    ensure!(
        raw.len() <= 4 * 1024 * 1024,
        "generated scope admission exceeds bound"
    );
    let input = generated_publication::input_source(&command, &serde_json::from_str(&raw)?)?;
    let publication = json!({"operation_id":operation,"payload_digest":command.digest().as_str(),
        "receipt_digest":catabolic_core::sha256(serde_json::to_vec(&receipt)?),"admission_digest":input.admission_digest});
    Ok(Ancestry { input, publication })
}
