//! Merge/split use cases over the legacy catalog tables. Decisions come from
//! `playscale_core::identity`; this adapter loads the affected aggregates inside
//! the writer transaction and applies the returned moves literally.
//!
//! Legacy storage mapping: an edition has exactly one timeline whose ID is the
//! edition ID; a version is the set of an edition's files sharing one revision,
//! identified by its smallest file ID (other files are copies/occurrences).
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
        let versions: Vec<(String, String, bool)> = sqlx::query_as(
            "SELECT min(id),revision,max(generated) FROM media_files WHERE edition_id=? GROUP BY revision ORDER BY min(id)",
        )
        .bind(&id)
        .fetch_all(&mut *conn)
        .await?;
        out.push(Edition {
            id: id.clone(),
            label,
            timelines: vec![Timeline {
                id,
                versions: versions
                    .into_iter()
                    .map(|(file, revision, generated)| Version {
                        id: file.clone(),
                        origin: if generated {
                            Origin::Generated
                        } else {
                            Origin::Original
                        },
                        equivalence: Equivalence::Declared,
                        bindings: vec![Binding {
                            file_id: file,
                            revision,
                            part: 1,
                            start_ms: None,
                            end_ms: None,
                        }],
                    })
                    .collect(),
            }],
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
    // Triggers advanced the revision once per row change inside this uncommitted
    // transaction; the committed structural revision is exactly the planned one.
    sqlx::query("UPDATE items SET catalog_revision=? WHERE id=?")
        .bind(i64::try_from(plan.target_revision)?)
        .bind(&plan.target)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Legacy editions hold one timeline, so a fresh timeline is a fresh edition and
/// shares its ID. Whole-timeline moves out of a multi-timeline edition cannot occur.
fn legacy_split(mut plan: SplitPlan) -> Result<SplitPlan, CurationError> {
    for step in &mut plan.moves {
        match step {
            SplitMove::Edition { .. } => {}
            SplitMove::Versions {
                new_edition,
                new_timeline,
                ..
            } => *new_timeline = new_edition.clone(),
            SplitMove::Timelines { .. } => {
                return Err(CurationError::Storage(anyhow::anyhow!(
                    "legacy editions hold exactly one timeline"
                )));
            }
        }
    }
    Ok(plan)
}

pub async fn preview_split(
    db: &SqlitePool,
    request: &SplitRequest,
) -> Result<SplitPlan, CurationError> {
    let mut conn = db.acquire().await?;
    let current = load_aggregate(&mut conn, &request.item)
        .await?
        .ok_or_else(|| IdentityError::UnknownItem(request.item.clone()))?;
    legacy_split(identity::plan_split(&current, request, &mut new_id)?)
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
    let fresh = legacy_split(identity::commit_split(reviewed, current.as_ref(), taken)?)?;
    let current = current.expect("commit_split requires the source");
    let receipt = receipt(&mut tx, "catalog:split", &fresh).await?;
    apply_split(&mut tx, &current, &fresh).await?;
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
    let revisions: BTreeMap<&str, &str> = current
        .editions
        .iter()
        .flat_map(|e| &e.timelines)
        .flat_map(|t| &t.versions)
        .map(|v| (v.id.as_str(), v.bindings[0].revision.as_str()))
        .collect();
    for step in &plan.moves {
        match step {
            SplitMove::Edition { edition } => {
                let moved = sqlx::query("UPDATE editions SET item_id=? WHERE id=? AND item_id=?")
                    .bind(&plan.new_item)
                    .bind(edition)
                    .bind(&plan.item)
                    .execute(&mut **tx)
                    .await?;
                anyhow::ensure!(moved.rows_affected() == 1, "edition moved concurrently");
            }
            SplitMove::Versions {
                from_timeline,
                new_edition,
                versions,
                ..
            } => {
                sqlx::query("INSERT INTO editions (id,item_id,label) SELECT ?,?,label FROM editions WHERE id=?")
                    .bind(new_edition).bind(&plan.new_item).bind(from_timeline).execute(&mut **tx).await?;
                for version in versions {
                    let revision = revisions
                        .get(version.as_str())
                        .ok_or_else(|| anyhow::anyhow!("unknown version"))?;
                    sqlx::query(
                        "UPDATE media_files SET edition_id=? WHERE edition_id=? AND revision=?",
                    )
                    .bind(new_edition)
                    .bind(from_timeline)
                    .bind(revision)
                    .execute(&mut **tx)
                    .await?;
                }
            }
            SplitMove::Timelines { .. } => anyhow::bail!("legacy editions hold one timeline"),
        }
    }
    sqlx::query("UPDATE renditions SET item_id=? WHERE item_id=? AND file_id IN (SELECT f.id FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=?)")
        .bind(&plan.new_item).bind(&plan.item).bind(&plan.new_item).execute(&mut **tx).await?;
    sqlx::query("UPDATE items SET catalog_revision=? WHERE id=?")
        .bind(i64::try_from(plan.expected_revision.saturating_add(1))?)
        .bind(&plan.item)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE items SET catalog_revision=1 WHERE id=?")
        .bind(&plan.new_item)
        .execute(&mut **tx)
        .await?;
    Ok(())
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
