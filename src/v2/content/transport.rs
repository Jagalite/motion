//! Explicit transport admission. Original-only consumers keep `file_facts`.
//! Caller MUST retain begin_catalog_application's current native writer fence.
use super::*;
use anyhow::{Context, ensure};
use motion_catalog_backend::{occurrence_bridge::OccurrenceMapping, version_proof};
use serde_json::Value;

/// A current exact output proof does not itself grant inherited scope.
struct GeneratedTransportProof {
    revision: String,
    version: String,
    catalog: String,
    incarnation: String,
    generation: i64,
    mapping: OccurrenceMapping,
}
impl GeneratedTransportProof {
    async fn admit(
        self,
        conn: &mut sqlx::SqliteConnection,
        principal: &Principal,
    ) -> Result<Option<FileFacts>, Problem> {
        let scope = core::catalog_scope(principal);
        if matches!(scope, core::CatalogScope::Nothing) || principal.policy.rating_restricted() {
            return Ok(None);
        }
        // Independent All output authority survives removal of inherited
        // membership. Restricted callers require explicit current contract 2.
        let library_ids = if matches!(scope, core::CatalogScope::All) {
            Default::default()
        } else {
            super::generated_membership::libraries(
                conn,
                self.mapping.motion_file["id"]
                    .as_str()
                    .ok_or_else(|| Problem::internal("generated file ID missing"))?,
                &self.version,
                &self.catalog,
                &self.incarnation,
                self.generation,
                &self.mapping,
                match &scope {
                    core::CatalogScope::Libraries(ids) => ids,
                    _ => unreachable!("All and Nothing handled above"),
                },
            )
            .await?
        };
        Ok(scope.admits(library_ids.iter()).then(|| FileFacts {
            revision: self.revision,
            library_ids,
        }))
    }
}

pub(super) async fn facts(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    file: &str,
) -> Result<Option<FileFacts>, Problem> {
    let generated: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM media_files WHERE id=? AND generated=1)")
            .bind(file)
            .fetch_one(&mut *conn)
            .await?;
    if !generated || !super::super::playback::native_bound(conn).await? {
        return super::file_facts(conn, file).await;
    }
    if matches!(core::catalog_scope(principal), core::CatalogScope::Nothing)
        || principal.policy.rating_restricted()
    {
        return Ok(None);
    }
    match generated_in(conn, file).await.map_err(Problem::internal)? {
        Some(proof) => proof.admit(conn, principal).await,
        None => Ok(None),
    }
}

pub(super) async fn version_ready(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    version: &str,
) -> Result<bool, Problem> {
    if super::super::playback::native_version_ready(conn, version).await? {
        return Ok(true);
    }
    if matches!(core::catalog_scope(principal), core::CatalogScope::Nothing)
        || principal.policy.rating_restricted()
    {
        return Ok(false);
    }
    let file:Option<String>=sqlx::query_scalar("SELECT b.file_id FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 WHERE v.id=? AND v.origin='generated'").bind(version).fetch_optional(&mut *conn).await?;
    let Some(file) = file else {
        return Ok(false);
    };
    match generated_in(conn, &file).await.map_err(Problem::internal)? {
        Some(proof) if proof.version == version => {
            Ok(proof.admit(conn, principal).await?.is_some())
        }
        _ => Ok(false),
    }
}

async fn generated_in(
    conn: &mut sqlx::SqliteConnection,
    file: &str,
) -> anyhow::Result<Option<GeneratedTransportProof>> {
    // Every descriptive owner, mapped head and whole-file binding must still
    // match the producer's independently validated current native proof.
    type Row = (
        String,
        String,
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
    );
    let rows: Vec<Row> = sqlx::query_as("SELECT s.catalog_id,s.incarnation,s.generation,p.version_id,p.timeline_id,e.item_id,e.id,p.manifest_json,q.mapping_json,p.native_version_revision,p.native_timeline_revision FROM catabolic_projection_state s JOIN catabolic_projection_versions p ON p.generation=s.generation JOIN media_versions v ON v.id=p.version_id AND v.revision=p.motion_version_revision JOIN timelines t ON t.id=p.timeline_id AND t.id=v.timeline_id AND t.revision=p.motion_timeline_revision JOIN editions e ON e.id=t.edition_id JOIN catabolic_projection_items i ON i.native_id=e.item_id AND i.generation=s.generation JOIN version_files b ON b.version_id=v.id JOIN media_files f ON f.id=b.file_id JOIN catabolic_projection_files q ON q.file_id=f.id AND q.generation=s.generation WHERE s.singleton=1 AND f.id=? AND f.generated=1 AND f.native_history=0 AND f.available=1 AND q.available=1 AND v.origin='generated' AND p.ready=1 AND p.native_timeline_revision=p.native_current_timeline_revision AND p.native_version_revision>0 AND p.native_timeline_revision>0 AND b.part=1 AND b.start_ms IS NULL AND b.end_ms IS NULL AND b.file_revision=f.revision AND NOT EXISTS(SELECT 1 FROM version_files other WHERE other.version_id=v.id AND other.part<>1) AND json_type(i.metadata_json,'$.retirement')='null' LIMIT 2")
        .bind(file).fetch_all(&mut *conn).await?;
    if rows.is_empty() {
        return Ok(None);
    }
    ensure!(rows.len() == 1, "generated transport binding is ambiguous");
    let (
        catalog,
        incarnation,
        generation,
        version,
        timeline,
        work,
        edition,
        envelope,
        encoded,
        version_head,
        timeline_head,
    ) = &rows[0];
    ensure!(
        envelope.len() <= 8 * 1024 * 1024 && encoded.len() <= 4 * 1024 * 1024,
        "generated transport proof exceeds bound"
    );
    let origin = version_proof::origin_in(conn, version, catalog, incarnation, *generation).await?;
    ensure!(
        origin == version_proof::ProofOrigin::ReceiptVerifiedGenerated,
        "generated transport has no receipt owner"
    );
    let mapping: OccurrenceMapping = serde_json::from_str(encoded)?;
    ensure!(
        mapping.motion_file["id"] == file
            && mapping.motion_file["generated"] == 1
            && mapping.motion_file["edition_id"] == *edition,
        "generated transport occurrence changed"
    );
    let request = catabolic_store::catalog_projection::VersionLineageRequest {
        work_item_id: work.clone(),
        edition_item_id: format!("motion.edition:{edition}"),
        version_item_id: mapping.association.item_id.clone(),
        version_id: version.clone(),
    };
    let envelope: Value = serde_json::from_str(envelope)?;
    let proof = version_proof::read(
        &envelope,
        catalog,
        incarnation,
        *generation,
        &request,
        origin,
    )?;
    ensure!(
        proof.head["version_revision"] == *version_head
            && proof.head["timeline_revision"] == *timeline_head
            && proof.head["timeline_id"] == *timeline,
        "generated transport heads changed"
    );
    let bindings = proof.version["version"]["bindings"]
        .as_array()
        .context("generated transport bindings missing")?;
    ensure!(
        bindings.len() == 1
            && bindings[0]["file"] == serde_json::to_value(&mapping.physical.exact)?,
        "generated transport exact output differs"
    );
    let duration = mapping.motion_file["duration_seconds"]
        .as_f64()
        .filter(|d| d.is_finite() && *d > 0.0)
        .context("generated duration missing")?
        * 1_000_000.0;
    if bindings[0]["source"]["kind"] != "whole_file"
        || bindings[0]["source"]["duration_us"]
            .as_i64()
            .map(|d| d as f64)
            != Some(duration)
        || proof.head["timeline_coverage"]["start_us"] != 0
        || proof.head["timeline_coverage"]["end_us"]
            .as_i64()
            .map(|d| d as f64)
            != Some(duration)
        || bindings[0]["timeline_interval"]["start_us"] != 0
        || bindings[0]["timeline_interval"]["end_us"]
            .as_i64()
            .map(|d| d as f64)
            != Some(duration)
    {
        // A legitimate edited slice remains descriptive, not this cohort.
        return Ok(None);
    }
    ensure!(
        mapping.version_files.len() == 1
            && mapping.version_files[0]["version_id"] == *version
            && mapping.version_files[0]["file_id"] == file
            && mapping.version_files[0]["start_ms"].is_null()
            && mapping.version_files[0]["end_ms"].is_null(),
        "generated transport mapped binding changed"
    );
    let revision = mapping.motion_file["revision"]
        .as_str()
        .context("generated transport revision missing")?
        .to_owned();
    let current: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM media_files WHERE id=? AND revision=? AND available=1)",
    )
    .bind(file)
    .bind(&revision)
    .fetch_one(&mut *conn)
    .await?;
    ensure!(current, "generated transport revision changed");
    Ok(Some(GeneratedTransportProof {
        revision,
        version: version.clone(),
        catalog: catalog.clone(),
        incarnation: incarnation.clone(),
        generation: *generation,
        mapping,
    }))
}
