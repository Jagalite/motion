//! Component evidence adapter: sidecar subtitle observations and the derived
//! component view of a timeline (`playscale_core::components`).
use playscale_core::components::{self as core, Component, Kind, Observed, Roles};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::BTreeMap;

/// Inside scan publication: replace the media file's sidecar subtitle set
/// with the names in its directory that `core::sidecar_subtitle` recognizes.
pub(crate) async fn publish_sidecars(
    conn: &mut SqliteConnection,
    file: &str,
    relative: &str,
    subtitles: &BTreeMap<String, Vec<(String, String)>>,
    complete: &std::collections::BTreeSet<String>,
) -> anyhow::Result<()> {
    let dir = playscale_core::scan::parent(relative);
    let stem = std::path::Path::new(relative)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    // The previous set is replaced only when the directory was listed
    // completely; otherwise unproven sidecars are retained.
    if complete.contains(dir) {
        sqlx::query("DELETE FROM sidecar_subtitles WHERE file_id=?")
            .bind(file)
            .execute(&mut *conn)
            .await?;
    }
    for (name, fingerprint) in subtitles.get(dir).into_iter().flatten() {
        let Some(sidecar) = core::sidecar_subtitle(stem, name) else {
            continue;
        };
        sqlx::query("INSERT OR REPLACE INTO sidecar_subtitles VALUES (?,?,?,?,?,?,?)")
            .bind(file)
            .bind(name)
            .bind(fingerprint)
            .bind(&sidecar.language)
            .bind(sidecar.roles.forced)
            .bind(sidecar.roles.hearing_impaired)
            .bind(&sidecar.format)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Components of a timeline derived from its versions' bound files: embedded
/// audio/subtitle tracks from probes and recognized sidecar subtitles. Each
/// occurrence pins the bound file revision. Read-only, one snapshot.
pub async fn timeline_components(
    db: &SqlitePool,
    timeline: &str,
) -> anyhow::Result<Vec<Component>> {
    let mut tx = db.begin().await?;
    let bound: Vec<(String, String, String, String)> = sqlx::query_as(
        // Evidence only from an occurrence holding the pinned (reviewed)
        // revision: the bound file if unchanged, else a copy with that content.
        "SELECT v.id,o.id,b.file_revision,o.tracks_json FROM media_versions v JOIN version_files b ON b.version_id=v.id JOIN media_files o ON o.id=coalesce((SELECT f.id FROM media_files f WHERE f.id=b.file_id AND f.revision=b.file_revision),(SELECT f.id FROM media_files f WHERE f.revision=b.file_revision ORDER BY f.available DESC,f.id LIMIT 1)) WHERE v.timeline_id=? ORDER BY v.id,b.part",
    )
    .bind(timeline)
    .fetch_all(&mut *tx)
    .await?;
    let mut versions: BTreeMap<String, Vec<Observed>> = BTreeMap::new();
    for (version, file, revision, tracks_json) in bound {
        let tracks: Vec<crate::db::Track> = serde_json::from_str(&tracks_json).unwrap_or_default();
        let entry = versions.entry(version).or_default();
        for track in tracks {
            let kind = match track.kind.as_str() {
                "audio" => Kind::Audio,
                "subtitle" => Kind::Subtitle,
                _ => continue,
            };
            entry.push(Observed {
                file_id: file.clone(),
                file_revision: revision.clone(),
                kind,
                language: track.language.clone(),
                title: track.title.clone(),
                roles: Roles {
                    forced: track.forced.unwrap_or(false),
                    hearing_impaired: track.hearing_impaired.unwrap_or(false),
                    commentary: track.commentary.unwrap_or(false),
                },
                default: track.default_track.unwrap_or(false),
                track_index: Some(track.index),
                sidecar: None,
            });
        }
        let sidecars: Vec<(String, Option<String>, bool, bool)> = sqlx::query_as(
            "SELECT name,language,forced,hearing_impaired FROM sidecar_subtitles WHERE file_id=? ORDER BY name",
        )
        .bind(&file)
        .fetch_all(&mut *tx)
        .await?;
        for (name, language, forced, hearing_impaired) in sidecars {
            entry.push(Observed {
                file_id: file.clone(),
                file_revision: revision.clone(),
                kind: Kind::Subtitle,
                language,
                title: None,
                roles: Roles {
                    forced,
                    hearing_impaired,
                    commentary: false,
                },
                default: false,
                track_index: None,
                sidecar: Some(name),
            });
        }
    }
    tx.commit().await?;
    let versions: Vec<(String, Vec<Observed>)> = versions.into_iter().collect();
    Ok(core::derive(timeline, &versions))
}
