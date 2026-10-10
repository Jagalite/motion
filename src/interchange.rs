//! `motion_export_v1` interchange adapter. Matching and planning are
//! `playscale_core::interchange`; this module gathers facts, exports and
//! applies a reviewed plan in one writer transaction with a receipt.
use crate::{App, curation::Receipt, new_id, now};
use playscale_core::{
    interchange::{
        self as core, Action, Export, ExportedCollection, ExportedEntry, ExportedPlaylist,
        ExportedWork, ImportPlan, InterchangeError, Segment, WorkFacts,
    },
    metadata::Contribution,
};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub enum ImportError {
    Rejected(InterchangeError),
    Storage(anyhow::Error),
}
impl From<anyhow::Error> for ImportError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e)
    }
}
impl From<sqlx::Error> for ImportError {
    fn from(e: sqlx::Error) -> Self {
        Self::Storage(e.into())
    }
}

/// Facts for every live work: identities, original content revisions,
/// local contribution and timelines with their pinned revisions.
async fn facts(conn: &mut SqliteConnection) -> anyhow::Result<Vec<WorkFacts>> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM items WHERE id NOT IN (SELECT alias_id FROM item_aliases) ORDER BY id",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let external_ids = sqlx::query_as::<_, (String, String)>(
            "SELECT source,external_id FROM metadata_documents WHERE item_id=? AND external_id IS NOT NULL",
        )
        .bind(&id)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect();
        let local: Option<(i64, String)> = sqlx::query_as(
            "SELECT revision,document_json FROM metadata_documents WHERE item_id=? AND source='local'",
        )
        .bind(&id)
        .fetch_optional(&mut *conn)
        .await?;
        #[allow(clippy::type_complexity)]
        let pinned: Vec<(String, String, String, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT t.id,v.id,b.file_revision,b.part,b.start_ms,b.end_ms FROM timelines t JOIN editions e ON e.id=t.edition_id JOIN media_versions v ON v.timeline_id=t.id JOIN version_files b ON b.version_id=v.id WHERE e.item_id=? AND v.origin='original' ORDER BY t.id,v.id,b.part",
        )
        .bind(&id)
        .fetch_all(&mut *conn)
        .await?;
        // Timeline -> version -> ordered segments.
        let mut timelines: BTreeMap<String, BTreeMap<String, Vec<Segment>>> = BTreeMap::new();
        for (timeline, version, revision, part, start, end) in &pinned {
            timelines
                .entry(timeline.clone())
                .or_default()
                .entry(version.clone())
                .or_default()
                .push(Segment {
                    revision: revision.clone(),
                    part: u32::try_from(*part)?,
                    start_ms: start.map(u64::try_from).transpose()?,
                    end_ms: end.map(u64::try_from).transpose()?,
                });
        }
        out.push(WorkFacts {
            content_revisions: pinned.into_iter().map(|(_, _, r, ..)| r).collect(),
            external_ids,
            local_revision: local
                .as_ref()
                .map_or(0, |(r, _)| u64::try_from(*r).unwrap_or(0)),
            local: local
                .map(|(_, doc)| serde_json::from_str::<Contribution>(&doc))
                .transpose()?,
            timelines: timelines
                .into_iter()
                .map(|(t, versions)| (t, versions.into_values().collect()))
                .collect(),
            id,
        });
    }
    Ok(out)
}

async fn names(conn: &mut SqliteConnection, profile: &str) -> anyhow::Result<BTreeSet<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT name FROM collections WHERE profile_id=?1 UNION SELECT name FROM playlists WHERE profile_id=?1",
    )
    .bind(profile)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect())
}

/// Export a profile's curation: works with local metadata or referenced by
/// its manual collections or playlists.
pub async fn export(db: &SqlitePool, profile: &str) -> anyhow::Result<Export> {
    let mut tx = db.begin().await?;
    let all = facts(&mut tx).await?;
    let by_id: BTreeMap<&str, &WorkFacts> = all.iter().map(|f| (f.id.as_str(), f)).collect();
    let collections: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,name FROM collections WHERE profile_id=? AND kind='manual' ORDER BY name,id",
    )
    .bind(profile)
    .fetch_all(&mut *tx)
    .await?;
    let playlists: Vec<(String, String)> =
        sqlx::query_as("SELECT id,name FROM playlists WHERE profile_id=? ORDER BY name,id")
            .bind(profile)
            .fetch_all(&mut *tx)
            .await?;
    let mut works: Vec<ExportedWork> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut add = |id: &str, works: &mut Vec<ExportedWork>| -> Option<usize> {
        if let Some(i) = index.get(id) {
            return Some(*i);
        }
        let f = by_id.get(id)?;
        works.push(ExportedWork {
            title: String::new(),
            external_ids: f.external_ids.iter().cloned().collect(),
            content_revisions: f.content_revisions.iter().cloned().collect(),
            local: f.local.clone(),
        });
        index.insert(id.to_string(), works.len() - 1);
        Some(works.len() - 1)
    };
    for f in all.iter().filter(|f| f.local.is_some()) {
        add(&f.id, &mut works);
    }
    let mut exported_collections = Vec::new();
    for (id, name) in collections {
        let members: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT coalesce(a.item_id,c.item_id) AS member FROM collection_items c LEFT JOIN item_aliases a ON a.alias_id=c.item_id WHERE c.collection_id=? ORDER BY member",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await?;
        // Retired members resolve to their surviving work (above); indices
        // are deduplicated in case two members now share one work.
        let mut indices: Vec<usize> = Vec::new();
        for member in &members {
            if let Some(i) = add(member, &mut works)
                && !indices.contains(&i)
            {
                indices.push(i);
            }
        }
        exported_collections.push(ExportedCollection {
            name,
            members: indices,
        });
    }
    let mut exported_playlists = Vec::new();
    for (id, name) in playlists {
        let entries: Vec<(String, String)> = sqlx::query_as(
            "SELECT p.timeline_id,e.item_id FROM playlist_entries p JOIN timelines t ON t.id=p.timeline_id JOIN editions e ON e.id=t.edition_id WHERE p.playlist_id=? ORDER BY p.position",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await?;
        let mut out = Vec::new();
        for (timeline, item) in entries {
            let Some(work) = add(&item, &mut works) else {
                continue;
            };
            // The first original version's ordered segments identify the
            // timeline; one without an original version cannot be matched.
            let Some(segments) = by_id
                .get(item.as_str())
                .and_then(|f| f.timelines.iter().find(|(t, _)| *t == timeline))
                .and_then(|(_, versions)| versions.first().cloned())
            else {
                continue;
            };
            out.push(ExportedEntry { work, segments });
        }
        exported_playlists.push(ExportedPlaylist { name, entries: out });
    }
    // Titles help operators read warnings; they are not used for matching.
    for (id, i) in &index {
        let title: Option<String> = sqlx::query_scalar("SELECT title FROM items WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        works[*i].title = title.unwrap_or_default();
    }
    tx.commit().await?;
    Ok(Export {
        format: core::FORMAT.into(),
        works,
        collections: exported_collections,
        playlists: exported_playlists,
    })
}

pub async fn preview_import(
    db: &SqlitePool,
    profile: &str,
    export: &Export,
) -> Result<ImportPlan, ImportError> {
    let mut tx = db.begin().await?;
    let facts = facts(&mut tx).await?;
    let names = names(&mut tx, profile).await?;
    tx.commit().await?;
    core::plan_import(export, &facts, profile, &names).map_err(ImportError::Rejected)
}

/// Apply a reviewed import plan; recomputed inside the writer transaction so
/// any intervening change makes it stale. Original media are never touched.
pub async fn commit_import(
    app: &App,
    export: &Export,
    plan: &ImportPlan,
) -> Result<Receipt, ImportError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let facts = facts(&mut tx).await?;
    let names = names(&mut tx, &plan.profile).await?;
    core::commit_import(plan, export, &facts, &names).map_err(ImportError::Rejected)?;
    for action in &plan.actions {
        match action {
            Action::SetLocal {
                work,
                expected_revision,
                contribution,
            } => {
                let document = serde_json::to_string(contribution).map_err(anyhow::Error::from)?;
                let next = i64::try_from(expected_revision + 1).map_err(anyhow::Error::from)?;
                sqlx::query("INSERT INTO metadata_documents VALUES (?,'local',?,NULL,?,?) ON CONFLICT(item_id,source) DO UPDATE SET revision=excluded.revision,document_json=excluded.document_json,updated_at=excluded.updated_at")
                    .bind(work).bind(next).bind(document).bind(now()).execute(&mut *tx).await?;
                crate::metadata::project_title(&mut tx, work).await?;
            }
            Action::CreateCollection { name, members } => {
                let id = new_id();
                sqlx::query("INSERT INTO collections VALUES (?,1,?,?,'manual',NULL)")
                    .bind(&id)
                    .bind(&plan.profile)
                    .bind(name)
                    .execute(&mut *tx)
                    .await?;
                for member in members {
                    sqlx::query("INSERT INTO collection_items VALUES (?,?)")
                        .bind(&id)
                        .bind(member)
                        .execute(&mut *tx)
                        .await?;
                }
            }
            Action::CreatePlaylist { name, timelines } => {
                let id = new_id();
                sqlx::query("INSERT INTO playlists VALUES (?,1,?,?)")
                    .bind(&id)
                    .bind(&plan.profile)
                    .bind(name)
                    .execute(&mut *tx)
                    .await?;
                for (position, timeline) in timelines.iter().enumerate() {
                    sqlx::query("INSERT INTO playlist_entries VALUES (?,?,?,?)")
                        .bind(&id)
                        .bind(i64::try_from(position).map_err(anyhow::Error::from)?)
                        .bind(new_id())
                        .bind(timeline)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
    }
    let receipt = Receipt {
        id: new_id(),
        kind: "interchange:import".into(),
        created_at: now(),
    };
    sqlx::query("INSERT INTO catalog_receipts VALUES (?,?,?,?)")
        .bind(&receipt.id)
        .bind(&receipt.kind)
        .bind(receipt.created_at)
        .bind(serde_json::to_string(plan).map_err(anyhow::Error::from)?)
        .execute(&mut *tx)
        .await?;
    crate::search::refresh(&mut tx, 500).await?;
    tx.commit().await?;
    Ok(receipt)
}
