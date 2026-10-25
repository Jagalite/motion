//! Exact native preparation identity checks shared by render admission and
//! private retry admission. A successful check does not grant input bytes or
//! native publication: the existing worker must reacquire those owners.
use anyhow::Context;

pub(crate) async fn validate_admission_in(
    runtime: &crate::native_catalog::Runtime,
    conn: &mut sqlx::SqliteConnection,
    job: &crate::processing::ProcessingJob,
) -> anyhow::Result<()> {
    // Caller retains a current projection and the Motion writer throughout.
    let raw: String = sqlx::query_scalar(
        "SELECT capture_json FROM catabolic_processing_admissions WHERE job_id=?",
    )
    .bind(&job.id)
    .fetch_one(&mut *conn)
    .await?;
    let admission: serde_json::Value = serde_json::from_str(&raw)?;
    let identity: (String, String) = sqlx::query_as(
        "SELECT catalog_id,incarnation FROM catabolic_projection_state WHERE singleton=1",
    )
    .fetch_one(&mut *conn)
    .await?;
    anyhow::ensure!(
        admission["contract_version"] == 1
            && admission["job_id"].as_str() == Some(job.id.as_str())
            && admission["catalog_id"].as_str() == Some(identity.0.as_str())
            && admission["incarnation"].as_str() == Some(identity.1.as_str()),
        "native admission belongs to another job or catalog incarnation"
    );
    let preparation: (String, String, String) = sqlx::query_as(
        "SELECT recipe_digest,tracks_json,timeline_id FROM preparations WHERE job_id=?",
    )
    .bind(&job.id)
    .fetch_one(&mut *conn)
    .await?;
    anyhow::ensure!(
        admission["recipe_digest"].as_str() == Some(preparation.0.as_str())
            && admission["tracks"] == serde_json::from_str::<serde_json::Value>(&preparation.1)?
            && admission["timeline_id"].as_str() == Some(preparation.2.as_str()),
        "native preparation differs from captured source admission"
    );
    let saved: motion_catalog_backend::occurrence_bridge::OccurrenceMapping =
        serde_json::from_value(admission["occurrence"].clone())?;
    anyhow::ensure!(
        saved.motion_file["id"].as_str() == Some(job.source_file_id.as_str())
            && saved.motion_file["revision"].as_str() == Some(job.source_revision.as_str()),
        "native render source differs from admission"
    );
    motion_catalog_backend::source_lifecycle::require_active_in(
        &mut *conn,
        &saved.source.library_id,
    )
    .await?;
    let projected: String = sqlx::query_scalar("SELECT f.mapping_json FROM catabolic_projection_files f JOIN catabolic_projection_state p ON p.singleton=1 AND p.generation=f.generation WHERE f.file_id=? AND f.available=1")
        .bind(&job.source_file_id).fetch_one(&mut *conn).await?;
    anyhow::ensure!(
        serde_json::from_str::<motion_catalog_backend::occurrence_bridge::OccurrenceMapping>(
            &projected
        )? == saved,
        "native render source projection changed"
    );
    runtime
        .with_host(move |host| {
            let version = admission["version_id"]
                .as_str()
                .context("captured native source version missing")?;
            let manifest = host
                .projection_version_manifest(host.catalog_generation()?, &[version.to_owned()])?;
            anyhow::ensure!(
                manifest["manifest"] == admission["manifest"]["manifest"],
                "native source version/timeline changed before snapshot"
            );
            let current =
                motion_catalog_backend::occurrence_bridge::project_with_host(host, &[saved])?;
            anyhow::ensure!(
                current.files.len() == 1 && current.files[0]["available"] == 1,
                "native source occurrence is unavailable"
            );
            Ok(())
        })
        .await?;
    Ok(())
}
