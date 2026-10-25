//! Read-only generated publication facts. The caller retains its authorization
//! writer; receipt/history evidence is not a byte grant or permission to ACK.
use super::{journal, matching_reserved_job};
use anyhow::{Context, ensure};
use serde_json::{Value, json};

pub(crate) struct CurrentRendition {
    pub output_file: String,
    pub output_revision: String,
    pub version_id: String,
    pub timeline_id: String,
    pub ready: bool,
}

async fn receipt_rendition_in(
    conn: &mut sqlx::SqliteConnection,
    job_id: &str,
) -> anyhow::Result<Option<ReceiptRendition>> {
    let row:Option<(String,String,String,i64)>=sqlx::query_as(
        "SELECT operation_id,receipt_json,output_json,attempt FROM catabolic_generated_publications WHERE job_id=? AND acknowledged=1 ORDER BY attempt DESC LIMIT 1")
        .bind(job_id).fetch_optional(&mut *conn).await?;
    let Some((operation, raw_receipt, raw_output, attempt)) = row else {
        return Ok(None);
    };
    ensure!(
        raw_receipt.len() <= 4 * 1024 * 1024 && raw_output.len() <= 4 * 1024 * 1024,
        "generated read proof exceeds bound"
    );
    let receipt: Value = serde_json::from_str(&raw_receipt)?;
    let output: Value = serde_json::from_str(&raw_output)?;
    let intent = journal::load_in(conn, &operation).await?;
    ensure!(
        receipt["contract_version"] == 1
            && receipt["attempt_envelope"]
                == serde_json::from_str::<Value>(intent.attempt.canonical_payload())?,
        "generated read receipt attempt changed"
    );
    let result = &receipt["result"];
    let request = &intent.command.payload()["request"];
    let job: crate::processing::ProcessingJob =
        sqlx::query_as("SELECT * FROM processing_jobs WHERE id=?")
            .bind(job_id)
            .fetch_one(&mut *conn)
            .await?;
    let observed = json!({"id":job.id,"attempt":job.attempt,"source_file_id":job.source_file_id,
        "source_revision":job.source_revision,"recipe":job.recipe,"backend":job.backend,"video_profile":job.video_profile});
    let reserved = matching_reserved_job(&request["provenance"]["job"], &observed)?;
    ensure!(
        job.attempt == attempt
            && job.phase == "completed"
            && result["provenance"] == request["provenance"],
        "generated read job disposition changed"
    );
    let source = motion_catalog_backend::generated_source::mapping_in(conn)
        .await?
        .context("generated read source missing")?;
    ensure!(
        output["mapping"] == serde_json::to_value(&source)?,
        "generated read source mapping changed"
    );
    let mapping = motion_catalog_backend::generated_publication::mapping(
        &intent.command,
        result,
        &source,
        &output["probe"],
        receipt["committed_generation"]
            .as_i64()
            .context("generated committed generation missing")?,
    )?;
    let file = mapping.motion_file["id"]
        .as_str()
        .context("generated output ID missing")?
        .to_owned();
    let revision = result["output_sha256"]
        .as_str()
        .context("generated output revision missing")?
        .to_owned();
    let version = result["version_id"]
        .as_str()
        .context("generated version missing")?
        .to_owned();
    let timeline = result["timeline_id"]
        .as_str()
        .context("generated timeline missing")?
        .to_owned();
    ensure!(
        job.output_file_id.as_deref() == Some(file.as_str()),
        "generated read job output changed"
    );
    let recipe = json!({"version":1,"recipe":reserved["recipe"],"backend":reserved["backend"],"job_id":reserved["id"],"attempt":reserved["attempt"],"video_profile":reserved["video_profile"],"native_publication":operation});
    let saved:(String,String,String,String,String,String)=sqlx::query_as("SELECT id,file_id,file_revision,source_file_id,source_revision,recipe_json FROM renditions WHERE source='playscale' AND external_id=?")
        .bind(job_id).fetch_one(&mut *conn).await?;
    ensure!(
        saved.0 == format!("native-generated:{operation}")
            && saved.1 == file
            && saved.2 == revision
            && saved.3 == job.source_file_id
            && saved.4 == job.source_revision
            && serde_json::from_str::<Value>(&saved.5)? == recipe,
        "generated read rendition provenance changed"
    );
    let preparation: (String, String, String) = sqlx::query_as(
        "SELECT timeline_id,recipe_digest,tracks_json FROM preparations WHERE job_id=?",
    )
    .bind(job_id)
    .fetch_one(&mut *conn)
    .await?;
    ensure!(
        preparation.0 == timeline
            && request["provenance"]["recipe_digest"] == preparation.1
            && request["provenance"]["tracks"] == serde_json::from_str::<Value>(&preparation.2)?,
        "generated read preparation changed"
    );
    let admission: String = sqlx::query_scalar(
        "SELECT capture_json FROM catabolic_processing_admissions WHERE job_id=?",
    )
    .bind(job_id)
    .fetch_one(&mut *conn)
    .await?;
    ensure!(
        admission.len() <= 4 * 1024 * 1024,
        "generated read admission exceeds bound"
    );
    let admission: Value = serde_json::from_str(&admission)?;
    ensure!(
        request["preparation_digest"] == catabolic_core::sha256(serde_json::to_vec(&admission)?)
            && admission["job_id"] == job_id
            && admission["timeline_id"] == timeline,
        "generated read admission digest changed"
    );
    Ok(Some(ReceiptRendition {
        result: result.clone(),
        command: intent.command,
        mapping,
        file,
        revision,
        version,
        timeline,
    }))
}

struct ReceiptRendition {
    command: catabolic_core::catalog_command::CommandEnvelope,
    result: Value,
    mapping: motion_catalog_backend::occurrence_bridge::OccurrenceMapping,
    file: String,
    revision: String,
    version: String,
    timeline: String,
}

/// Validates mutable application preparation/rendition facts against the exact
/// acknowledged native receipt. This does not establish readiness or bytes.
pub(crate) async fn validate_prepared_rendition_in(
    conn: &mut sqlx::SqliteConnection,
    job_id: &str,
    output_file: &str,
    output_revision: &str,
    version_id: &str,
    timeline_id: &str,
) -> anyhow::Result<bool> {
    Ok(receipt_rendition_in(conn, job_id).await?.is_some_and(|r| {
        r.file == output_file
            && r.revision == output_revision
            && r.version == version_id
            && r.timeline == timeline_id
    }))
}

pub(crate) async fn current_rendition_in(
    runtime: &crate::native_catalog::Runtime,
    conn: &mut sqlx::SqliteConnection,
    job_id: &str,
) -> anyhow::Result<Option<CurrentRendition>> {
    let Some(ReceiptRendition {
        command,
        result,
        mapping,
        file,
        revision,
        version,
        timeline,
    }) = receipt_rendition_in(conn, job_id).await?
    else {
        return Ok(None);
    };
    let state:(String,String,i64)=sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1")
        .fetch_one(&mut *conn).await?;
    let projected:Option<(String,String)>=sqlx::query_as(
        "SELECT p.manifest_json,q.mapping_json FROM catabolic_projection_versions p JOIN media_versions v ON v.id=p.version_id JOIN timelines t ON t.id=v.timeline_id JOIN version_files b ON b.version_id=v.id AND b.part=1 JOIN media_files f ON f.id=b.file_id JOIN catabolic_projection_files q ON q.file_id=f.id WHERE p.version_id=? AND v.timeline_id=? AND f.id=? AND f.revision=? AND b.file_revision=f.revision AND b.start_ms IS NULL AND b.end_ms IS NULL AND NOT EXISTS(SELECT 1 FROM version_files x WHERE x.version_id=v.id AND x.part<>1) AND f.generated=1 AND f.native_history=0 AND f.available=1 AND q.available=1 AND p.ready=1 AND p.generation=? AND q.generation=? AND v.revision=p.motion_version_revision AND t.revision=p.motion_timeline_revision")
        .bind(&version).bind(&timeline).bind(&file).bind(&revision).bind(state.2).bind(state.2).fetch_optional(&mut *conn).await?;
    if let Some((_, encoded)) = &projected {
        ensure!(
            serde_json::from_str::<motion_catalog_backend::occurrence_bridge::OccurrenceMapping>(
                encoded
            )? == mapping,
            "generated read occurrence mapping changed"
        );
    }
    let work: String = sqlx::query_scalar(
        "SELECT e.item_id FROM editions e JOIN timelines t ON t.edition_id=e.id WHERE t.id=?",
    )
    .bind(&timeline)
    .fetch_one(&mut *conn)
    .await?;
    let version_for_host = version.clone();
    let ready = runtime
        .with_host(move |host| {
            let identity = host.identity()?;
            ensure!(
                identity["catalog_id"] == state.0
                    && identity["catalog_incarnation"] == state.1
                    && host.catalog_generation()? == state.2,
                "generated read current fence changed"
            );
            let owner = command.payload()["request"]["item"]["id"]
                .as_str()
                .context("generated owner missing")?;
            let history = host.historical_version(
                state.2,
                &version_for_host,
                owner,
                result["version_revision"]
                    .as_i64()
                    .context("generated receipt version head missing")?,
            )?;
            let retired = motion_catalog_backend::generated_publication::verify_historical(
                &command, &result, &history,
            )?;
            let Some((manifest, _)) = projected else {
                return Ok(false);
            };
            if retired {
                return Ok(false);
            }
            motion_catalog_backend::generated_publication::verify_current_version(
                host,
                state.2,
                &work,
                &command,
                &result,
                &serde_json::from_str::<Value>(&manifest)?,
            )?;
            Ok(true)
        })
        .await?;
    Ok(Some(CurrentRendition {
        output_file: file,
        output_revision: revision,
        version_id: version,
        timeline_id: timeline,
        ready,
    }))
}
