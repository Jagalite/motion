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
    for effect in core::sidecar_plan(file, relative, subtitles, complete) {
        match effect {
            core::SidecarEffect::Clear { file } => {
                sqlx::query("DELETE FROM sidecar_subtitles WHERE file_id=?")
                    .bind(file)
                    .execute(&mut *conn)
                    .await?;
            }
            core::SidecarEffect::Upsert {
                file,
                name,
                fingerprint,
                subtitle,
            } => {
                sqlx::query("INSERT OR REPLACE INTO sidecar_subtitles VALUES (?,?,?,?,?,?,?)")
                    .bind(file)
                    .bind(name)
                    .bind(fingerprint)
                    .bind(&subtitle.language)
                    .bind(subtitle.roles.forced)
                    .bind(subtitle.roles.hearing_impaired)
                    .bind(&subtitle.format)
                    .execute(&mut *conn)
                    .await?;
            }
        }
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
    let result = timeline_components_in(&mut tx, timeline).await?;
    tx.commit().await?;
    Ok(result)
}

/// Use the caller's read snapshot without acquiring another pool slot.
pub(crate) async fn timeline_components_in(
    conn: &mut SqliteConnection,
    timeline: &str,
) -> anyhow::Result<Vec<Component>> {
    let bound: Vec<(String, String, String, String)> = sqlx::query_as(
        // Evidence only from an occurrence holding the pinned (reviewed)
        // revision: the bound file if unchanged, else a copy with that content.
        "SELECT v.id,o.id,b.file_revision,o.tracks_json FROM media_versions v JOIN version_files b ON b.version_id=v.id JOIN media_files o ON o.id=coalesce((SELECT f.id FROM media_files f WHERE f.id=b.file_id AND f.revision=b.file_revision),(SELECT f.id FROM media_files f WHERE f.revision=b.file_revision ORDER BY f.available DESC,f.id LIMIT 1)) WHERE v.timeline_id=? ORDER BY v.id,b.part",
    )
    .bind(timeline)
    .fetch_all(&mut *conn)
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
        .fetch_all(&mut *conn)
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
    let versions: Vec<(String, Vec<Observed>)> = versions.into_iter().collect();
    Ok(core::derive(timeline, &versions))
}

#[cfg(test)]
mod sidecar_adapter_tests {
    use super::*;
    use std::collections::BTreeSet;

    #[tokio::test]
    async fn incomplete_coverage_keeps_unseen_rows_and_applies_observations_atomically() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE sidecar_subtitles (file_id TEXT, name TEXT, fingerprint TEXT, language TEXT, forced BOOLEAN, hearing_impaired BOOLEAN, format TEXT, PRIMARY KEY(file_id,name))")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sidecar_subtitles VALUES ('file','film.fr.srt','old-fr','fr',0,0,'srt'),('file','film.en.srt','old-en','en',0,0,'srt')")
            .execute(&pool).await.unwrap();
        let observations = BTreeMap::from([(
            "dir".to_owned(),
            vec![
                ("film.en.srt".to_owned(), "new-en".to_owned()),
                ("film.de.srt".to_owned(), "new-de".to_owned()),
            ],
        )]);
        let mut tx = pool.begin().await.unwrap();
        publish_sidecars(
            &mut tx,
            "file",
            "dir/film.mkv",
            &observations,
            &BTreeSet::new(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,fingerprint FROM sidecar_subtitles ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            [
                ("film.de.srt".into(), "new-de".into()),
                ("film.en.srt".into(), "new-en".into()),
                ("film.fr.srt".into(), "old-fr".into())
            ]
        );
        let mut tx = pool.begin().await.unwrap();
        publish_sidecars(
            &mut tx,
            "file",
            "dir/film.mkv",
            &observations,
            &BTreeSet::from(["dir".into()]),
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        let after_rollback: Vec<(String, String)> =
            sqlx::query_as("SELECT name,fingerprint FROM sidecar_subtitles ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(after_rollback, rows);
    }
}
