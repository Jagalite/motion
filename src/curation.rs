//! Catalog structure use cases: merge, split, file reassignment, version
//! creation and replacement confirmation. Decisions come from
//! `playscale_core::identity`; this adapter loads the affected aggregates inside
//! the writer transaction and applies the returned changes literally.
//!
//! Storage: `timelines` belong to editions, `media_versions` to timelines, and
//! `version_files` bind each part to a file record pinned at the reviewed content
//! revision. `media_files.edition_id` remains the file's edition; files sharing
//! a pinned revision are occurrences (copies) of that content.
use crate::{App, new_id, now};
use playscale_core::identity::{
    self, Aggregate, Binding, Edition, Equivalence, Id, IdentityError, MergePlan, MergeRequest,
    Origin, Resolved, SplitMove, SplitPlan, SplitRequest, Timeline, Version, Work,
};
use serde::Serialize;
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};
use std::collections::BTreeMap;

#[derive(Debug)]
pub enum CurationError {
    Rejected(IdentityError),
    Storage(anyhow::Error),
}
impl From<IdentityError> for CurationError {
    fn from(e: IdentityError) -> Self {
        Self::Rejected(e)
    }
}
impl From<anyhow::Error> for CurationError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e)
    }
}
impl From<sqlx::Error> for CurationError {
    fn from(e: sqlx::Error) -> Self {
        Self::Storage(e.into())
    }
}
impl std::fmt::Display for CurationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(e) => write!(f, "rejected: {e:?}"),
            Self::Storage(e) => write!(f, "storage: {e}"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub id: String,
    pub kind: String,
    pub created_at: i64,
}

/// Load one live work with its complete owned structure.
pub async fn load_aggregate(
    conn: &mut SqliteConnection,
    item: &str,
) -> anyhow::Result<Option<Aggregate>> {
    let Some(revision): Option<i64> = sqlx::query_scalar(
        "SELECT catalog_revision FROM items WHERE id=? AND id NOT IN (SELECT alias_id FROM item_aliases)",
    )
    .bind(item)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    // Every source contribution is part of the work's identity evidence. A
    // contribution without an external ID is encoded with an empty value so two
    // works that both carry the same source never merge silently.
    let external_ids = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT source,external_id FROM metadata_documents WHERE item_id=?",
    )
    .bind(item)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(source, value)| (source, value.unwrap_or_default()))
    .collect();
    let editions: Vec<(String, String)> =
        sqlx::query_as("SELECT id,label FROM editions WHERE item_id=? ORDER BY id")
            .bind(item)
            .fetch_all(&mut *conn)
            .await?;
    let mut out = Vec::new();
    for (id, label) in editions {
        let timeline_ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM timelines WHERE edition_id=? ORDER BY id")
                .bind(&id)
                .fetch_all(&mut *conn)
                .await?;
        let mut timelines = Vec::new();
        for timeline in timeline_ids {
            let rows: Vec<(String, String, String)> = sqlx::query_as(
                "SELECT id,origin,equivalence FROM media_versions WHERE timeline_id=? ORDER BY id",
            )
            .bind(&timeline)
            .fetch_all(&mut *conn)
            .await?;
            let mut versions = Vec::new();
            for (version, origin, equivalence) in rows {
                versions.push(Version {
                    bindings: bindings(conn, &version).await?,
                    id: version,
                    origin: enum_value(&origin)?,
                    equivalence: enum_value(&equivalence)?,
                });
            }
            timelines.push(Timeline {
                id: timeline,
                versions,
            });
        }
        out.push(Edition {
            id,
            label,
            timelines,
        });
    }
    Ok(Some(Aggregate {
        work: Work {
            id: item.into(),
            revision: u64::try_from(revision)?,
            external_ids,
        },
        editions: out,
    }))
}

fn enum_value<T: serde::de::DeserializeOwned>(name: &str) -> anyhow::Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(
        name.into(),
    ))?)
}
fn enum_name<T: Serialize>(value: &T) -> anyhow::Result<String> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .unwrap_or_default()
        .to_string())
}

async fn bindings(conn: &mut SqliteConnection, version: &str) -> anyhow::Result<Vec<Binding>> {
    type Row = (String, String, i64, Option<i64>, Option<i64>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT file_id,file_revision,part,start_ms,end_ms FROM version_files WHERE version_id=? ORDER BY part",
    )
    .bind(version)
    .fetch_all(&mut *conn)
    .await?;
    rows.into_iter()
        .map(|(file_id, revision, part, start, end)| {
            Ok(Binding {
                file_id,
                revision,
                part: u32::try_from(part)?,
                start_ms: start.map(u64::try_from).transpose()?,
                end_ms: end.map(u64::try_from).transpose()?,
            })
        })
        .collect()
}

async fn participants(
    conn: &mut SqliteConnection,
    ids: &[Id],
) -> anyhow::Result<BTreeMap<Id, Aggregate>> {
    let mut out = BTreeMap::new();
    for id in ids {
        if let Some(aggregate) = load_aggregate(conn, id).await? {
            out.insert(id.clone(), aggregate);
        }
    }
    Ok(out)
}

async fn aliases(conn: &mut SqliteConnection) -> anyhow::Result<BTreeMap<Id, Id>> {
    Ok(
        sqlx::query_as::<_, (String, String)>("SELECT alias_id,item_id FROM item_aliases")
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .collect(),
    )
}

async fn structure(conn: &mut SqliteConnection, item: &str) -> anyhow::Result<(String, usize)> {
    let kind: Option<String> =
        sqlx::query_scalar("SELECT media_type FROM item_structure WHERE item_id=?")
            .bind(item)
            .fetch_optional(&mut *conn)
            .await?;
    let children: i64 = sqlx::query_scalar("SELECT count(*) FROM item_structure WHERE parent_id=?")
        .bind(item)
        .fetch_one(&mut *conn)
        .await?;
    Ok((
        kind.unwrap_or_else(|| "unclassified".into()),
        usize::try_from(children)?,
    ))
}

async fn merge_decision(
    conn: &mut SqliteConnection,
    request: &MergeRequest,
) -> Result<(BTreeMap<Id, Aggregate>, BTreeMap<Id, Id>), CurationError> {
    let ids: Vec<Id> = request
        .sources
        .iter()
        .chain([&request.target])
        .cloned()
        .collect();
    let found = participants(conn, &ids).await?;
    let (target_kind, _) = structure(conn, &request.target).await?;
    let mut sources = Vec::new();
    for id in request.sources.iter().filter(|id| found.contains_key(*id)) {
        let (kind, children) = structure(conn, id).await?;
        sources.push((id.clone(), kind, children));
    }
    identity::merge_structure(&target_kind, &sources)?;
    Ok((found, aliases(conn).await?))
}

/// Read-only preview; commit rechecks every recorded revision.
pub async fn preview_merge(
    db: &SqlitePool,
    request: &MergeRequest,
) -> Result<MergePlan, CurationError> {
    let mut conn = db.acquire().await?;
    let (found, aliases) = merge_decision(&mut conn, request).await?;
    Ok(identity::plan_merge(&found, &aliases, request)?)
}

pub async fn commit_merge(app: &App, reviewed: &MergePlan) -> Result<Receipt, CurationError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let receipt = merge_in_transaction(&mut tx, reviewed).await?;
    tx.commit().await?;
    Ok(receipt)
}

/// Caller holds `App.jobs` and a reserved-writer transaction; nothing commits here.
pub(crate) async fn merge_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    reviewed: &MergePlan,
) -> Result<Receipt, CurationError> {
    let request = MergeRequest {
        sources: reviewed.retired.clone(),
        target: reviewed.target.clone(),
        expected: reviewed.expected.clone(),
    };
    let (found, aliases) = merge_decision(tx, &request).await?;
    let plan = identity::commit_merge(reviewed, &found, &aliases)?;
    let receipt = receipt(tx, "catalog:merge", &plan).await?;
    apply_merge(tx, &plan, &receipt.id).await?;
    crate::search::refresh(tx, 100).await?;
    Ok(receipt)
}

/// Plan a merge from revisions read inside the caller's writer transaction.
pub(crate) async fn plan_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    sources: Vec<Id>,
    target: Id,
) -> Result<MergePlan, CurationError> {
    let mut expected = BTreeMap::new();
    let ids: Vec<Id> = sources.iter().chain([&target]).cloned().collect();
    for id in &ids {
        if let Some(a) = load_aggregate(tx, id).await? {
            expected.insert(id.clone(), a.work.revision);
        }
    }
    let request = MergeRequest {
        sources,
        target,
        expected,
    };
    let (found, aliases) = merge_decision(tx, &request).await?;
    Ok(identity::plan_merge(&found, &aliases, &request)?)
}

async fn apply_merge(
    tx: &mut Transaction<'_, Sqlite>,
    plan: &MergePlan,
    receipt: &str,
) -> anyhow::Result<()> {
    for (edition, from) in &plan.moved_editions {
        let moved = sqlx::query("UPDATE editions SET item_id=? WHERE id=? AND item_id=?")
            .bind(&plan.target)
            .bind(edition)
            .bind(from)
            .execute(&mut **tx)
            .await?;
        anyhow::ensure!(moved.rows_affected() == 1, "edition moved concurrently");
    }
    for retired in &plan.retired {
        // Contributions move only when the target has none from that source;
        // otherwise the target's stays authoritative and the retired one remains
        // attached to the retired ID for explanation. Equal-source conflicts with
        // differing identities were rejected by the decision.
        sqlx::query("UPDATE metadata_documents SET item_id=? WHERE item_id=? AND source NOT IN (SELECT source FROM metadata_documents WHERE item_id=?)")
            .bind(&plan.target).bind(retired).bind(&plan.target).execute(&mut **tx).await?;
        // The NFO origin moves with its contribution (it moved only if the
        // target had no NFO contribution, hence no origin of its own).
        sqlx::query("UPDATE nfo_origins SET item_id=? WHERE item_id=? AND NOT EXISTS (SELECT 1 FROM nfo_origins WHERE item_id=?)")
            .bind(&plan.target).bind(retired).bind(&plan.target).execute(&mut **tx).await?;
        sqlx::query("UPDATE artwork_contributions SET item_id=? WHERE item_id=? AND (role,source) NOT IN (SELECT role,source FROM artwork_contributions WHERE item_id=?)")
            .bind(&plan.target).bind(retired).bind(&plan.target).execute(&mut **tx).await?;
        sqlx::query("UPDATE renditions SET item_id=? WHERE item_id=?")
            .bind(&plan.target)
            .bind(retired)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM item_structure WHERE item_id=?")
            .bind(retired)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE item_aliases SET item_id=? WHERE item_id=?")
            .bind(&plan.target)
            .bind(retired)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO item_aliases VALUES (?,?,?)")
            .bind(retired)
            .bind(&plan.target)
            .bind(receipt)
            .execute(&mut **tx)
            .await?;
    }
    // The strongest match state (including a manual pin) moves with the
    // identities the merge carried to the target.
    let mut states = Vec::new();
    for id in plan.retired.iter().chain([&plan.target]) {
        let state: String = sqlx::query_scalar("SELECT match_state FROM items WHERE id=?")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
        states.push(serde_json::from_value(serde_json::Value::String(state))?);
    }
    let state = serde_json::to_value(playscale_core::matching::merged_state(&states))?;
    sqlx::query("UPDATE items SET match_state=? WHERE id=?")
        .bind(state.as_str())
        .bind(&plan.target)
        .execute(&mut **tx)
        .await?;
    crate::metadata::project_title(tx, &plan.target).await?;
    // Triggers advanced the revision once per row change inside this uncommitted
    // transaction; the committed structural revision is exactly the planned one.
    sqlx::query("UPDATE items SET catalog_revision=? WHERE id=?")
        .bind(i64::try_from(plan.target_revision)?)
        .bind(&plan.target)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn preview_split(
    db: &SqlitePool,
    request: &SplitRequest,
) -> Result<SplitPlan, CurationError> {
    let mut conn = db.acquire().await?;
    let current = load_aggregate(&mut conn, &request.item)
        .await?
        .ok_or_else(|| IdentityError::UnknownItem(request.item.clone()))?;
    Ok(identity::plan_split(&current, request, &mut new_id)?)
}

pub async fn commit_split(app: &App, reviewed: &SplitPlan) -> Result<Receipt, CurationError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let current = load_aggregate(&mut tx, &reviewed.item).await?;
    let taken = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM items WHERE id=?")
        .bind(&reviewed.new_item)
        .fetch_one(&mut *tx)
        .await?
        > 0;
    let fresh = identity::commit_split(reviewed, current.as_ref(), taken)?;
    let current = current.expect("commit_split requires the source");
    let receipt = receipt(&mut tx, "catalog:split", &fresh).await?;
    apply_split(&mut tx, &current, &fresh).await?;
    crate::search::refresh(&mut tx, 100).await?;
    tx.commit().await?;
    Ok(receipt)
}

async fn apply_split(
    tx: &mut Transaction<'_, Sqlite>,
    current: &Aggregate,
    plan: &SplitPlan,
) -> anyhow::Result<()> {
    let (kind, media_type): (String, Option<String>) = sqlx::query_as(
        "SELECT i.kind,s.media_type FROM items i LEFT JOIN item_structure s ON s.item_id=i.id WHERE i.id=?",
    )
    .bind(&plan.item)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO items (id,title,kind) VALUES (?,?,?)")
        .bind(&plan.new_item)
        .bind(&plan.new_title)
        .bind(&kind)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO item_origins VALUES (?,?)")
        .bind(&plan.new_item)
        .bind(&plan.new_title)
        .execute(&mut **tx)
        .await?;
    // Root kinds carry over; a split episode/season has no hierarchy slot yet.
    if let Some(media_type) = media_type.filter(|k| k == "movie" || k == "unclassified") {
        sqlx::query(
            "INSERT INTO item_structure (item_id,media_type,parent_id,number) VALUES (?,?,NULL,NULL)",
        )
        .bind(&plan.new_item)
        .bind(media_type)
        .execute(&mut **tx)
        .await?;
    }
    // Where each timeline and version currently lives.
    let mut edition_of_timeline = BTreeMap::new();
    let mut versions_by_timeline: BTreeMap<&str, Vec<&Version>> = BTreeMap::new();
    let mut labels = BTreeMap::new();
    for edition in &current.editions {
        labels.insert(edition.id.as_str(), edition.label.as_str());
        for timeline in &edition.timelines {
            edition_of_timeline.insert(timeline.id.as_str(), edition.id.as_str());
            versions_by_timeline.insert(&timeline.id, timeline.versions.iter().collect());
        }
    }
    let mut created = std::collections::BTreeSet::new();
    // (from edition, new edition) -> versions moved between them.
    let mut moved: BTreeMap<(&str, &str), Vec<&Version>> = BTreeMap::new();
    for step in &plan.moves {
        let (from_edition, new_edition) = match step {
            SplitMove::Edition { edition } => {
                let changed = sqlx::query("UPDATE editions SET item_id=? WHERE id=? AND item_id=?")
                    .bind(&plan.new_item)
                    .bind(edition)
                    .bind(&plan.item)
                    .execute(&mut **tx)
                    .await?;
                anyhow::ensure!(changed.rows_affected() == 1, "edition moved concurrently");
                continue;
            }
            SplitMove::Timelines {
                from_edition,
                new_edition,
                ..
            } => (from_edition.as_str(), new_edition.as_str()),
            SplitMove::Versions {
                from_timeline,
                new_edition,
                ..
            } => (
                *edition_of_timeline
                    .get(from_timeline.as_str())
                    .ok_or_else(|| anyhow::anyhow!("unknown timeline"))?,
                new_edition.as_str(),
            ),
        };
        if created.insert(new_edition) {
            sqlx::query("INSERT INTO editions (id,item_id,label) VALUES (?,?,?)")
                .bind(new_edition)
                .bind(&plan.new_item)
                .bind(labels.get(from_edition).copied().unwrap_or_default())
                .execute(&mut **tx)
                .await?;
        }
        let entry = moved.entry((from_edition, new_edition)).or_default();
        match step {
            SplitMove::Timelines { timelines, .. } => {
                for timeline in timelines {
                    sqlx::query("UPDATE timelines SET edition_id=? WHERE id=? AND edition_id=?")
                        .bind(new_edition)
                        .bind(timeline)
                        .bind(from_edition)
                        .execute(&mut **tx)
                        .await?;
                    entry.extend(
                        versions_by_timeline
                            .get(timeline.as_str())
                            .into_iter()
                            .flatten(),
                    );
                }
            }
            SplitMove::Versions {
                from_timeline,
                new_timeline,
                versions,
                ..
            } => {
                sqlx::query("INSERT INTO timelines (id,edition_id) VALUES (?,?)")
                    .bind(new_timeline)
                    .bind(new_edition)
                    .execute(&mut **tx)
                    .await?;
                for version in versions {
                    sqlx::query(
                        "UPDATE media_versions SET timeline_id=? WHERE id=? AND timeline_id=?",
                    )
                    .bind(new_timeline)
                    .bind(version)
                    .bind(from_timeline)
                    .execute(&mut **tx)
                    .await?;
                }
                entry.extend(
                    versions_by_timeline
                        .get(from_timeline.as_str())
                        .into_iter()
                        .flatten()
                        .filter(|v| versions.contains(&v.id)),
                );
            }
            SplitMove::Edition { .. } => {}
        }
    }
    // Files follow the content they represent: each moved version's bound files,
    // plus copies of its pinned content unless a version left behind in the old
    // edition still pins that content.
    for ((from_edition, new_edition), versions) in moved {
        let moved_ids: std::collections::BTreeSet<&str> =
            versions.iter().map(|v| v.id.as_str()).collect();
        let remaining: std::collections::BTreeSet<&str> = current
            .editions
            .iter()
            .filter(|e| e.id == from_edition)
            .flat_map(|e| &e.timelines)
            .flat_map(|t| &t.versions)
            .filter(|v| !moved_ids.contains(v.id.as_str()))
            .flat_map(|v| &v.bindings)
            .map(|b| b.revision.as_str())
            .collect();
        for binding in versions.iter().flat_map(|v| &v.bindings) {
            sqlx::query("UPDATE media_files SET edition_id=? WHERE id=? AND edition_id=?")
                .bind(new_edition)
                .bind(&binding.file_id)
                .bind(from_edition)
                .execute(&mut **tx)
                .await?;
            if !remaining.contains(binding.revision.as_str()) {
                sqlx::query(
                    "UPDATE media_files SET edition_id=? WHERE edition_id=? AND revision=?",
                )
                .bind(new_edition)
                .bind(from_edition)
                .bind(&binding.revision)
                .execute(&mut **tx)
                .await?;
            }
        }
    }
    ensure_consistent(tx).await?;
    sqlx::query("UPDATE renditions SET item_id=? WHERE item_id=? AND file_id IN (SELECT f.id FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=?)")
        .bind(&plan.new_item).bind(&plan.item).bind(&plan.new_item).execute(&mut **tx).await?;
    sqlx::query("UPDATE items SET catalog_revision=? WHERE id=?")
        .bind(i64::try_from(plan.expected_revision + 1)?)
        .bind(&plan.item)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE items SET catalog_revision=1 WHERE id=?")
        .bind(&plan.new_item)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Bindings must stay within their file's edition; checked before commit.
async fn ensure_consistent(conn: &mut SqliteConnection) -> anyhow::Result<()> {
    let mismatched: i64 = sqlx::query_scalar("SELECT count(*) FROM version_edition_mismatch")
        .fetch_one(&mut *conn)
        .await?;
    anyhow::ensure!(mismatched == 0, "version binding left its file's edition");
    Ok(())
}

/// Create an edition with its default timeline (same ID).
pub async fn create_edition(
    conn: &mut SqliteConnection,
    id: &str,
    item: &str,
    label: &str,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO editions (id,item_id,label) VALUES (?,?,?)")
        .bind(id)
        .bind(item)
        .bind(label)
        .execute(&mut *conn)
        .await?;
    sqlx::query("INSERT INTO timelines (id,edition_id) VALUES (?,?)")
        .bind(id)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The edition's default timeline (same ID), created if the edition has none
/// by that ID; otherwise its first timeline.
pub(crate) async fn timeline_for(
    conn: &mut SqliteConnection,
    edition: &str,
) -> anyhow::Result<String> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM timelines WHERE edition_id=? ORDER BY id<>edition_id,id LIMIT 1",
    )
    .bind(edition)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    sqlx::query("INSERT INTO timelines (id,edition_id) VALUES (?,?)")
        .bind(edition)
        .bind(edition)
        .execute(&mut *conn)
        .await?;
    Ok(edition.into())
}

/// Create a single-part version binding a file at its current revision in a
/// timeline. `attach_version` decides whether the equivalence permits joining.
pub(crate) async fn create_version(
    conn: &mut SqliteConnection,
    timeline: &str,
    file: &str,
    origin: Origin,
    equivalence: Equivalence,
) -> Result<String, CurationError> {
    let occupied: i64 =
        sqlx::query_scalar("SELECT count(*) FROM media_versions WHERE timeline_id=?")
            .bind(timeline)
            .fetch_one(&mut *conn)
            .await?;
    let existing = Timeline {
        id: timeline.into(),
        versions: (0..occupied)
            .map(|i| Version {
                id: i.to_string(),
                origin,
                equivalence,
                bindings: vec![],
            })
            .collect(),
    };
    identity::attach_version(&existing, equivalence)?;
    let revision: String = sqlx::query_scalar("SELECT revision FROM media_files WHERE id=?")
        .bind(file)
        .fetch_one(&mut *conn)
        .await?;
    let id = new_id();
    sqlx::query("INSERT INTO media_versions (id,timeline_id,origin,equivalence) VALUES (?,?,?,?)")
        .bind(&id)
        .bind(timeline)
        .bind(enum_name(&origin)?)
        .bind(enum_name(&equivalence)?)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO version_files (version_id,part,file_id,file_revision) VALUES (?,1,?,?)",
    )
    .bind(&id)
    .bind(file)
    .bind(revision)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// Operator reassignment of one file to another edition of the same work.
/// Its content's version moves with it unless a copy can keep representing the
/// version in the old edition, in which case the file gets its own declared
/// version. Multipart versions are never torn apart.
pub async fn reassign_file(
    conn: &mut SqliteConnection,
    file: &str,
    target_edition: &str,
) -> Result<(), CurationError> {
    let (from_edition, revision): (String, String) =
        sqlx::query_as("SELECT edition_id,revision FROM media_files WHERE id=?")
            .bind(file)
            .fetch_one(&mut *conn)
            .await?;
    if from_edition == target_edition {
        return Ok(());
    }
    let bound: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT b.version_id,(SELECT count(*) FROM version_files p WHERE p.version_id=b.version_id),b.file_revision FROM version_files b WHERE b.file_id=?",
    )
    .bind(file)
    .fetch_optional(&mut *conn)
    .await?;
    // A copy must hold the binding's pinned (reviewed) content, not whatever
    // the moved file holds now.
    let copy: Option<String> = match &bound {
        Some((_, _, pinned)) => {
            sqlx::query_scalar(
                "SELECT id FROM media_files WHERE edition_id=? AND revision=? AND id<>? ORDER BY id LIMIT 1",
            )
            .bind(&from_edition)
            .bind(pinned)
            .bind(file)
            .fetch_optional(&mut *conn)
            .await?
        }
        None => None,
    };
    let _ = revision;
    let bound_versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM version_files WHERE file_id=?")
            .bind(file)
            .fetch_one(&mut *conn)
            .await?;
    let decision = identity::reassignment(&identity::ReassignFacts {
        bound_versions: usize::try_from(bound_versions).unwrap_or(usize::MAX),
        bound: bound
            .as_ref()
            .map(|(v, parts, _)| (v.clone(), usize::try_from(*parts).unwrap_or(usize::MAX))),
        pinned_copy_remains: copy.is_some(),
    })?;
    let generated: bool = sqlx::query_scalar("SELECT generated FROM media_files WHERE id=?")
        .bind(file)
        .fetch_one(&mut *conn)
        .await?;
    let origin = if generated {
        Origin::Generated
    } else {
        Origin::Original
    };
    sqlx::query("UPDATE media_files SET edition_id=? WHERE id=?")
        .bind(target_edition)
        .bind(file)
        .execute(&mut *conn)
        .await?;
    let timeline = timeline_for(conn, target_edition).await?;
    match decision {
        identity::Reassignment::RebindToCopyAndDeclareNew { version } => {
            sqlx::query("UPDATE version_files SET file_id=? WHERE version_id=? AND file_id=?")
                .bind(copy.as_deref())
                .bind(&version)
                .bind(file)
                .execute(&mut *conn)
                .await?;
            create_version(&mut *conn, &timeline, file, origin, Equivalence::Declared).await?;
        }
        // An operator declaration is explicit equivalence, so the version may
        // join an occupied timeline (see `identity::attach_version`).
        identity::Reassignment::MoveVersion { version } => {
            sqlx::query("UPDATE media_versions SET timeline_id=?,equivalence='declared',revision=revision+1 WHERE id=?")
                .bind(&timeline)
                .bind(&version)
                .execute(&mut *conn)
                .await?;
        }
        identity::Reassignment::DeclareNew => {
            create_version(&mut *conn, &timeline, file, origin, Equivalence::Declared).await?;
        }
    }
    ensure_consistent(conn).await?;
    Ok(())
}

/// Confirm that replaced bytes still represent a version's timeline.
/// `reviewed` maps each part to the content revision the operator inspected.
pub async fn confirm_replacement(
    app: &App,
    version: &str,
    expected_revision: u64,
    reviewed: &BTreeMap<u32, String>,
) -> Result<u64, CurationError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let (revision, origin, equivalence): (i64, String, String) =
        sqlx::query_as("SELECT revision,origin,equivalence FROM media_versions WHERE id=?")
            .bind(version)
            .fetch_one(&mut *tx)
            .await?;
    let current = Version {
        id: version.into(),
        origin: enum_value(&origin)?,
        equivalence: enum_value(&equivalence)?,
        bindings: bindings(&mut tx, version).await?,
    };
    let occurrences = occurrences(&mut tx, &current.bindings).await?;
    let (next, next_revision) = identity::confirm_replacement(
        &current,
        u64::try_from(revision).map_err(anyhow::Error::from)?,
        expected_revision,
        reviewed,
        &occurrences,
    )?;
    for binding in &next.bindings {
        sqlx::query("UPDATE version_files SET file_revision=? WHERE version_id=? AND part=?")
            .bind(&binding.revision)
            .bind(version)
            .bind(i64::from(binding.part))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE media_versions SET revision=?,equivalence=? WHERE id=?")
        .bind(i64::try_from(next_revision).map_err(anyhow::Error::from)?)
        .bind(enum_name(&next.equivalence)?)
        .bind(version)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(next_revision)
}

/// Occurrences relevant to some bindings: the bound files themselves and every
/// file holding a pinned revision.
async fn occurrences(
    conn: &mut SqliteConnection,
    bindings: &[Binding],
) -> anyhow::Result<Vec<identity::Occurrence>> {
    let mut out = Vec::new();
    for binding in bindings {
        let rows: Vec<(String, String, bool)> = sqlx::query_as(
            "SELECT id,revision,available FROM media_files WHERE id=? OR revision=?",
        )
        .bind(&binding.file_id)
        .bind(&binding.revision)
        .fetch_all(&mut *conn)
        .await?;
        for (file_id, revision, available) in rows {
            out.push(identity::Occurrence {
                file_id,
                revision,
                available,
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VersionView {
    pub id: String,
    pub origin: Origin,
    pub equivalence: Equivalence,
    pub availability: identity::Availability,
    pub bindings: Vec<Binding>,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TimelineView {
    pub id: String,
    pub versions: Vec<VersionView>,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EditionView {
    pub id: String,
    pub label: String,
    pub timelines: Vec<TimelineView>,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StructureView {
    pub item_id: String,
    pub revision: u64,
    pub editions: Vec<EditionView>,
}

/// Side-effect-free read of a work's structure with derived availability, for
/// the API and the in-process UI query facade. Retired IDs resolve to the live
/// work. One read snapshot; access policy is applied by the caller (A08).
pub async fn read_structure(db: &SqlitePool, id: &str) -> anyhow::Result<Option<StructureView>> {
    let mut tx = db.begin().await?;
    let id =
        match sqlx::query_scalar::<_, String>("SELECT item_id FROM item_aliases WHERE alias_id=?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        {
            Some(target) => target,
            None => id.to_string(),
        };
    let Some(aggregate) = load_aggregate(&mut tx, &id).await? else {
        return Ok(None);
    };
    let mut editions = Vec::new();
    for edition in aggregate.editions {
        let mut timelines = Vec::new();
        for timeline in edition.timelines {
            let mut versions = Vec::new();
            for version in timeline.versions {
                let occurrences = occurrences(&mut tx, &version.bindings).await?;
                versions.push(VersionView {
                    availability: identity::version_availability(&version.bindings, &occurrences),
                    id: version.id,
                    origin: version.origin,
                    equivalence: version.equivalence,
                    bindings: version.bindings,
                });
            }
            timelines.push(TimelineView {
                id: timeline.id,
                versions,
            });
        }
        editions.push(EditionView {
            id: edition.id,
            label: edition.label,
            timelines,
        });
    }
    tx.commit().await?;
    Ok(Some(StructureView {
        item_id: aggregate.work.id,
        revision: aggregate.work.revision,
        editions,
    }))
}

async fn receipt<T: Serialize>(
    conn: &mut SqliteConnection,
    kind: &str,
    plan: &T,
) -> anyhow::Result<Receipt> {
    let receipt = Receipt {
        id: new_id(),
        kind: kind.into(),
        created_at: now(),
    };
    sqlx::query("INSERT INTO catalog_receipts VALUES (?,?,?,?)")
        .bind(&receipt.id)
        .bind(kind)
        .bind(receipt.created_at)
        .bind(serde_json::to_string(plan)?)
        .execute(&mut *conn)
        .await?;
    Ok(receipt)
}

/// Resolve a possibly retired work ID to its live work.
pub async fn resolve(db: &SqlitePool, id: &str) -> anyhow::Result<Option<Resolved>> {
    let live: Option<String> = sqlx::query_scalar(
        "SELECT id FROM items WHERE id=? AND id NOT IN (SELECT alias_id FROM item_aliases)",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    if live.is_some() {
        return Ok(Some(Resolved::Live(id.into())));
    }
    let target: Option<String> =
        sqlx::query_scalar("SELECT item_id FROM item_aliases WHERE alias_id=?")
            .bind(id)
            .fetch_optional(db)
            .await?;
    Ok(target.map(|to| Resolved::Alias {
        from: id.into(),
        to,
    }))
}

// ---------------------------------------------------------------------------
// Explicit structure operations

async fn expect_revision(
    conn: &mut SqliteConnection,
    item: &str,
    expected: u64,
) -> Result<Aggregate, CurationError> {
    let aggregate = load_aggregate(conn, item)
        .await?
        .ok_or_else(|| IdentityError::UnknownItem(item.into()))?;
    if aggregate.work.revision != expected {
        return Err(IdentityError::StaleRevision(item.into()).into());
    }
    Ok(aggregate)
}

/// Add a timeline to one of the work's editions (e.g. an alternate ordering
/// or a separately tracked cut), fenced on the work's structural revision.
pub async fn create_timeline(
    app: &App,
    item: &str,
    edition: &str,
    expected_revision: u64,
    duration_ms: Option<u64>,
) -> Result<String, CurationError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let aggregate = expect_revision(&mut tx, item, expected_revision).await?;
    if !aggregate.editions.iter().any(|e| e.id == edition) {
        return Err(
            IdentityError::InvalidBindings("edition belongs to another work".into()).into(),
        );
    }
    let id = new_id();
    sqlx::query("INSERT INTO timelines (id,edition_id,duration_ms) VALUES (?,?,?)")
        .bind(&id)
        .bind(edition)
        .bind(
            duration_ms
                .map(i64::try_from)
                .transpose()
                .map_err(anyhow::Error::from)?,
        )
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

/// Explicitly attach a version (one or more parts) to a timeline.
pub async fn create_version_explicit(
    app: &App,
    timeline: &str,
    label: &str,
    request: &identity::VersionRequest,
    expected_revision: u64,
) -> Result<String, CurationError> {
    if label.len() > 200 {
        return Err(IdentityError::InvalidTitle.into());
    }
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let (edition, item): (String, String) = sqlx::query_as(
        "SELECT t.edition_id,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
    )
    .bind(timeline)
    .fetch_one(&mut *tx)
    .await?;
    let aggregate = expect_revision(&mut tx, &item, expected_revision).await?;
    let current = aggregate
        .editions
        .iter()
        .flat_map(|e| &e.timelines)
        .find(|t| t.id == timeline)
        .cloned()
        .ok_or_else(|| IdentityError::UnknownItem(timeline.into()))?;
    let mut files = BTreeMap::new();
    let mut generated = Vec::new();
    let mut others = Vec::new();
    for binding in &request.bindings {
        let file: Option<(String, String, bool)> =
            sqlx::query_as("SELECT edition_id,revision,generated FROM media_files WHERE id=?")
                .bind(&binding.file_id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((edition, revision, is_generated)) = file {
            files.insert(binding.file_id.clone(), (edition, revision));
            generated.push(is_generated);
        }
        type Row = (String, String, String, i64, Option<i64>, Option<i64>);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT v.timeline_id,b.file_id,b.file_revision,b.part,b.start_ms,b.end_ms FROM version_files b JOIN media_versions v ON v.id=b.version_id WHERE b.file_id=? AND v.timeline_id<>?",
        )
        .bind(&binding.file_id)
        .bind(timeline)
        .fetch_all(&mut *tx)
        .await?;
        for (t, file_id, revision, part, start, end) in rows {
            others.push((
                t,
                Binding {
                    file_id,
                    revision,
                    part: u32::try_from(part).map_err(anyhow::Error::from)?,
                    start_ms: start
                        .map(u64::try_from)
                        .transpose()
                        .map_err(anyhow::Error::from)?,
                    end_ms: end
                        .map(u64::try_from)
                        .transpose()
                        .map_err(anyhow::Error::from)?,
                },
            ));
        }
    }
    identity::plan_version(&current, &edition, request, &files, &others)?;
    let id = new_id();
    let origin = if !generated.is_empty() && generated.iter().all(|g| *g) {
        "generated"
    } else {
        "original"
    };
    sqlx::query(
        "INSERT INTO media_versions (id,timeline_id,label,origin,equivalence) VALUES (?,?,?,?,?)",
    )
    .bind(&id)
    .bind(timeline)
    .bind(label.trim())
    .bind(origin)
    .bind(enum_name(&request.equivalence)?)
    .execute(&mut *tx)
    .await?;
    for binding in &request.bindings {
        sqlx::query("INSERT INTO version_files (version_id,part,file_id,file_revision,start_ms,end_ms) VALUES (?,?,?,?,?,?)")
            .bind(&id)
            .bind(i64::from(binding.part))
            .bind(&binding.file_id)
            .bind(&binding.revision)
            .bind(binding.start_ms.map(i64::try_from).transpose().map_err(anyhow::Error::from)?)
            .bind(binding.end_ms.map(i64::try_from).transpose().map_err(anyhow::Error::from)?)
            .execute(&mut *tx)
            .await?;
    }
    ensure_consistent(&mut tx).await?;
    tx.commit().await?;
    Ok(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderKind {
    Aired,
    Dvd,
    Absolute,
    Custom,
}

pub async fn create_order_group(
    app: &App,
    edition: &str,
    name: &str,
    kind: OrderKind,
) -> Result<String, CurationError> {
    if name.trim().is_empty() || name.len() > 200 {
        return Err(IdentityError::InvalidTitle.into());
    }
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let id = new_id();
    sqlx::query("INSERT INTO order_groups (id,edition_id,name,kind) VALUES (?,?,?,?)")
        .bind(&id)
        .bind(edition)
        .bind(name.trim())
        .bind(enum_name(&kind)?)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

/// Place a timeline at a position in an order group, fenced on the group's
/// revision. The timeline's work must descend from the group's work.
pub async fn place_timeline(
    app: &App,
    timeline: &str,
    group: &str,
    position: u32,
    expected_group_revision: u64,
) -> Result<u64, CurationError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let (revision, owner): (i64, String) = sqlx::query_as(
        "SELECT g.revision,e.item_id FROM order_groups g JOIN editions e ON e.id=g.edition_id WHERE g.id=?",
    )
    .bind(group)
    .fetch_one(&mut *tx)
    .await?;
    if u64::try_from(revision).map_err(anyhow::Error::from)? != expected_group_revision {
        return Err(IdentityError::StaleRevision(group.into()).into());
    }
    let item: String = sqlx::query_scalar(
        "SELECT e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
    )
    .bind(timeline)
    .fetch_one(&mut *tx)
    .await?;
    // The work itself and up to two hierarchy ancestors (season, series).
    let mut ancestors = vec![item.clone()];
    let mut current = item;
    for _ in 0..2 {
        let parent: Option<Option<String>> =
            sqlx::query_scalar("SELECT parent_id FROM item_structure WHERE item_id=?")
                .bind(&current)
                .fetch_optional(&mut *tx)
                .await?;
        match parent.flatten() {
            Some(p) => {
                ancestors.push(p.clone());
                current = p;
            }
            None => break,
        }
    }
    let occupied: BTreeMap<u32, String> = sqlx::query_as::<_, (i64, String)>(
        "SELECT order_position,id FROM timelines WHERE order_group_id=? AND order_position IS NOT NULL",
    )
    .bind(group)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .filter_map(|(p, id)| u32::try_from(p).ok().map(|p| (p, id)))
    .collect();
    identity::place_in_order(timeline, &ancestors, &owner, position, &occupied)?;
    sqlx::query(
        "UPDATE timelines SET order_group_id=?,order_position=?,revision=revision+1 WHERE id=?",
    )
    .bind(group)
    .bind(i64::from(position))
    .bind(timeline)
    .execute(&mut *tx)
    .await?;
    let next = expected_group_revision + 1;
    sqlx::query("UPDATE order_groups SET revision=? WHERE id=?")
        .bind(i64::try_from(next).map_err(anyhow::Error::from)?)
        .bind(group)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(next)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RelationshipView {
    pub id: String,
    pub revision: u64,
    pub source_item_id: String,
    pub target_item_id: String,
    pub kind: identity::RelationshipKind,
    pub position: Option<u32>,
}

pub async fn create_relationship(
    app: &App,
    source: &str,
    target: &str,
    kind: identity::RelationshipKind,
    position: Option<u32>,
) -> Result<RelationshipView, CurationError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    for id in [source, target] {
        if load_aggregate(&mut tx, id).await?.is_none() {
            return Err(IdentityError::UnknownItem(id.into()).into());
        }
    }
    let existing: Vec<(String, String, identity::RelationshipKind)> =
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT source_item_id,target_item_id,kind FROM item_relationships",
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(s, t, k)| Ok((s, t, enum_value(&k)?)))
        .collect::<anyhow::Result<_>>()?;
    identity::validate_relationship(source, target, kind, &existing)?;
    let view = RelationshipView {
        id: new_id(),
        revision: 1,
        source_item_id: source.into(),
        target_item_id: target.into(),
        kind,
        position,
    };
    sqlx::query("INSERT INTO item_relationships (id,source_item_id,target_item_id,kind,position) VALUES (?,?,?,?,?)")
        .bind(&view.id)
        .bind(source)
        .bind(target)
        .bind(enum_name(&kind)?)
        .bind(position.map(i64::from))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(view)
}

pub async fn delete_relationship(
    app: &App,
    id: &str,
    expected_revision: u64,
) -> Result<(), CurationError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM item_relationships WHERE id=?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if u64::try_from(revision).map_err(anyhow::Error::from)? != expected_revision {
        return Err(IdentityError::StaleRevision(id.into()).into());
    }
    sqlx::query("DELETE FROM item_relationships WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Relationships touching a work (as source or target), live works only.
pub async fn relationships(db: &SqlitePool, item: &str) -> anyhow::Result<Vec<RelationshipView>> {
    type Row = (String, i64, String, String, String, Option<i64>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id,revision,source_item_id,target_item_id,kind,position FROM item_relationships WHERE source_item_id=? OR target_item_id=? ORDER BY kind,position,id",
    )
    .bind(item)
    .bind(item)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|(id, revision, source, target, kind, position)| {
            Ok(RelationshipView {
                id,
                revision: u64::try_from(revision)?,
                source_item_id: source,
                target_item_id: target,
                kind: enum_value(&kind)?,
                position: position.map(u32::try_from).transpose()?,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FileView {
    pub id: String,
    pub edition_id: String,
    pub revision: String,
    pub available: bool,
    pub generated: bool,
    pub bytes: i64,
}

/// Physical occurrences belonging to a work's editions (source-relative paths
/// are deliberately not exposed here).
pub async fn item_files(db: &SqlitePool, item: &str) -> anyhow::Result<Vec<FileView>> {
    Ok(sqlx::query_as::<_, (String, String, String, bool, bool, i64)>(
        "SELECT f.id,f.edition_id,f.revision,f.available,f.generated,f.bytes FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=? ORDER BY f.id",
    )
    .bind(item)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|(id, edition_id, revision, available, generated, bytes)| FileView {
        id,
        edition_id,
        revision,
        available,
        generated,
        bytes,
    })
    .collect())
}
