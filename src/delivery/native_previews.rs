//! Exact native whole-original preview facts. These helpers consume a caller's
//! current native writer; neither cached facts nor the returned descriptor is
//! an authorization grant by itself.
use super::{Source, source_valid};
use crate::{
    execution,
    v2::{Problem, playback},
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

type Writer = sqlx::Transaction<'static, sqlx::Sqlite>;

#[derive(Clone)]
pub(crate) struct Facts {
    pub seconds: Option<f64>,
    pub tracks: String,
    pub commitment: Value,
    source: Source,
}
impl Facts {
    pub fn identity(&self) -> String {
        crate::v2::sha256(
            serde_json::to_string(&(self.seconds, &self.tracks))
                .expect("finite preview facts")
                .as_bytes(),
        )
    }
}

pub(crate) async fn facts_in(
    conn: &mut sqlx::SqliteConnection,
    file: &str,
    revision: &str,
) -> Result<Option<Facts>, Problem> {
    let Some(file_facts) = crate::v2::content::file_facts(conn, file).await? else {
        return Ok(None);
    };
    if file_facts.revision != revision {
        return Ok(None);
    }
    let versions: Vec<String> = sqlx::query_scalar("SELECT v.id FROM media_versions v JOIN version_files b ON b.version_id=v.id JOIN media_files f ON f.id=b.file_id WHERE b.file_id=? AND b.file_revision=? AND f.revision=b.file_revision AND f.generated=0 AND f.available=1 AND v.origin='original' AND b.part=1 AND b.start_ms IS NULL AND b.end_ms IS NULL AND NOT EXISTS(SELECT 1 FROM version_files o WHERE o.version_id=v.id AND o.part<>1) ORDER BY v.id LIMIT 201")
        .bind(file).bind(revision).fetch_all(&mut *conn).await?;
    if versions.len() > 200 {
        return Err(Problem::internal(
            "preview original binding inventory exceeds bound",
        ));
    }
    let mut ready = false;
    for version in versions {
        if playback::native_version_ready(conn, &version).await? {
            ready = true;
            break;
        }
    }
    if !ready {
        return Ok(None);
    }
    let encoded: Option<String> = sqlx::query_scalar("SELECT p.mapping_json FROM catabolic_projection_files p JOIN catabolic_projection_state s ON s.singleton=1 AND s.generation=p.generation WHERE p.file_id=? AND p.available=1")
        .bind(file).fetch_optional(&mut *conn).await?;
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    if encoded.len() > 4 * 1024 * 1024 {
        return Err(Problem::internal("preview occurrence exceeds bound"));
    }
    let mapping: motion_catalog_backend::occurrence_bridge::OccurrenceMapping =
        serde_json::from_str(&encoded).map_err(Problem::internal)?;
    if mapping.motion_file["id"] != file
        || mapping.motion_file["revision"] != revision
        || mapping.motion_file["generated"] != 0
    {
        return Err(Problem::internal("preview occurrence identity mismatch"));
    }
    let tracks = mapping.motion_file["tracks_json"]
        .as_str()
        .filter(|s| s.len() <= 1024 * 1024)
        .ok_or_else(|| Problem::internal("preview tracks unavailable"))?
        .to_owned();
    let _: Vec<crate::db::Track> = serde_json::from_str(&tracks).map_err(Problem::internal)?;
    let duration = &mapping.motion_file["duration_seconds"];
    let seconds = if duration.is_null() {
        None
    } else {
        Some(
            duration
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or_else(|| Problem::internal("invalid preview duration"))?,
        )
    };
    let source = Source {
        root: mapping.source.motion_root.clone().into(),
        relative: mapping.motion_file["relative_path"]
            .as_str()
            .ok_or_else(|| Problem::internal("preview path missing"))?
            .into(),
        root_identity: mapping.source.motion_root_identity.clone(),
        fingerprint: mapping.motion_file["fingerprint"]
            .as_str()
            .ok_or_else(|| Problem::internal("preview fingerprint missing"))?
            .into(),
    };
    Ok(Some(Facts {
        seconds,
        tracks,
        commitment: serde_json::to_value(mapping).map_err(Problem::internal)?,
        source,
    }))
}

/// The current writer and artifact admission follow actual blocking open,
/// including cancellation/late completion. Caller must reauthorize before use.
pub(crate) async fn open_in<A: Send + Sync + 'static>(
    tx: Writer,
    facts: &Facts,
    lease: Arc<execution::Lease>,
    admission: Arc<A>,
    stop: &CancellationToken,
) -> anyhow::Result<(std::fs::File, String)> {
    let source = facts.source.clone();
    let fingerprint = source.fingerprint.clone();
    let file = execution::prepare(
        lease,
        stop,
        Duration::from_secs(5),
        move || {
            let _admission = admission;
            let _writer = tx;
            source_valid(&source)
        },
        || {},
    )
    .await
    .ok_or_else(|| anyhow::anyhow!("preview source acquisition did not settle"))???;
    Ok((file, fingerprint))
}

pub(crate) async fn validate_path_in<A: Send + Sync + 'static>(
    tx: Writer,
    facts: &Facts,
    lease: Arc<execution::Lease>,
    admission: Arc<A>,
    stop: &CancellationToken,
) -> anyhow::Result<(Writer, bool)> {
    let source = facts.source.clone();
    execution::prepare(
        lease,
        stop,
        Duration::from_secs(5),
        move || {
            let _admission = admission;
            let current = source_valid(&source).is_ok();
            Ok::<_, anyhow::Error>((tx, current))
        },
        || {},
    )
    .await
    .ok_or_else(|| anyhow::anyhow!("preview final source check did not settle"))??
}
