//! Identification workflow persistence. Decisions come from
//! `playscale_core::matching`; this adapter supplies the observed file revision
//! and applies an accepted identification in the same writer transaction.
use crate::{App, curation, new_id, now};
use playscale_core::matching::{
    self, Attachment, Candidate, Decision, MatchError, MatchState, Proposal, ProposalAction, Status,
};
use serde::Serialize;
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

#[derive(Debug)]
pub enum MatchingError {
    Rejected(MatchError),
    Curation(curation::CurationError),
    /// The file shares its work with other versions; split it first.
    SplitRequired,
    /// Another work already holds the provider identity.
    IdentityTaken,
    NotFound,
    Storage(anyhow::Error),
}
impl From<MatchError> for MatchingError {
    fn from(e: MatchError) -> Self {
        Self::Rejected(e)
    }
}
impl From<anyhow::Error> for MatchingError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e)
    }
}
impl From<sqlx::Error> for MatchingError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound,
            e => Self::Storage(e.into()),
        }
    }
}
impl From<serde_json::Error> for MatchingError {
    fn from(e: serde_json::Error) -> Self {
        Self::Storage(e.into())
    }
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Pending => "pending",
        Status::Review => "review",
        Status::Accepted => "accepted",
        Status::Rejected => "rejected",
        Status::Deferred => "deferred",
        Status::Stale => "stale",
    }
}
fn status_value(name: &str) -> anyhow::Result<Status> {
    Ok(serde_json::from_value(serde_json::Value::String(
        name.into(),
    ))?)
}

pub(crate) type Row = (String, i64, String, String, String, String, Option<String>);
const COLUMNS: &str =
    "id,revision,file_id,file_revision,status,candidates_json,decision_json FROM match_proposals";

pub(crate) fn decode(row: Row) -> anyhow::Result<Proposal> {
    let (id, revision, file_id, file_revision, status, candidates, decision) = row;
    Ok(Proposal {
        id,
        revision: u64::try_from(revision)?,
        file_id,
        file_revision,
        status: status_value(&status)?,
        candidates: serde_json::from_str(&candidates)?,
        decision: decision.map(|d| serde_json::from_str(&d)).transpose()?,
    })
}

async fn store(conn: &mut SqliteConnection, p: &Proposal, insert: bool) -> anyhow::Result<()> {
    let candidates = serde_json::to_string(&p.candidates)?;
    let decision = p.decision.as_ref().map(serde_json::to_string).transpose()?;
    let revision = i64::try_from(p.revision)?;
    if insert {
        sqlx::query("INSERT INTO match_proposals VALUES (?,?,?,?,?,?,?,?,?)")
            .bind(&p.id)
            .bind(revision)
            .bind(&p.file_id)
            .bind(&p.file_revision)
            .bind(status_name(p.status))
            .bind(candidates)
            .bind(decision)
            .bind(now())
            .bind(now())
            .execute(&mut *conn)
            .await?;
    } else {
        sqlx::query("UPDATE match_proposals SET revision=?,status=?,candidates_json=?,decision_json=?,updated_at=? WHERE id=?")
            .bind(revision).bind(status_name(p.status)).bind(candidates).bind(decision).bind(now()).bind(&p.id)
            .execute(&mut *conn).await?;
    }
    Ok(())
}

pub async fn get(db: &SqlitePool, id: &str) -> Result<Proposal, MatchingError> {
    let row: Row = sqlx::query_as(&format!("SELECT {COLUMNS} WHERE id=?"))
        .bind(id)
        .fetch_one(db)
        .await?;
    Ok(decode(row)?)
}

/// Undecided proposals, oldest change first, with an ID tie-breaker.
pub async fn inbox(db: &SqlitePool, limit: i64) -> Result<Vec<Proposal>, MatchingError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} WHERE status IN ('pending','review','deferred') ORDER BY updated_at,id LIMIT ?"
    ))
    .bind(limit.clamp(1, 200))
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|r| decode(r).map_err(MatchingError::from))
        .collect()
}

/// Local candidates: other live works whose normalized title equals the
/// parsed filename title. Evidence only; nothing is written to the works.
async fn local_candidates(
    conn: &mut SqliteConnection,
    file_id: &str,
    relative_path: &str,
    own_item: &str,
) -> anyhow::Result<Vec<Candidate>> {
    let stem = std::path::Path::new(relative_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(relative_path);
    let parsed = matching::parse_name(stem);
    let wanted = matching::normalize_title(&parsed.title);
    if wanted.is_empty() {
        return Ok(vec![]);
    }
    let items: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,title FROM items WHERE id<>? AND id NOT IN (SELECT alias_id FROM item_aliases) ORDER BY id",
    )
    .bind(own_item)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::new();
    for (id, title) in items {
        if matching::normalize_title(&title) != wanted {
            continue;
        }
        let year: Option<i64> = sqlx::query_scalar(
            "SELECT json_extract(document_json,'$.values.release_year') FROM metadata_documents WHERE item_id=? AND json_extract(document_json,'$.values.release_year') IS NOT NULL ORDER BY source='local' DESC,source LIMIT 1",
        )
        .bind(&id)
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
        let mut reasons = vec!["title".to_string()];
        match (parsed.year, year) {
            (Some(a), Some(b)) if i64::from(a) == b => reasons.push("year".into()),
            (Some(_), Some(_)) => continue,
            _ => {}
        }
        out.push(Candidate {
            id: format!("local:{id}"),
            item_id: Some(id),
            title,
            external_id: None,
            reason_codes: reasons,
        });
        if out.len() == matching::MAX_CANDIDATES {
            break;
        }
    }
    let _ = file_id;
    Ok(out)
}

/// Raise or refresh the undecided proposal for a file's current revision.
pub async fn propose(app: &App, file_id: &str) -> Result<Proposal, MatchingError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let proposal = propose_in_transaction(&mut tx, file_id).await?;
    tx.commit().await?;
    Ok(proposal)
}

/// `propose` inside the caller's writer transaction; the caller holds
/// `App.jobs` and commits.
pub(crate) async fn propose_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    file_id: &str,
) -> Result<Proposal, MatchingError> {
    let (revision, path, item): (String, String, String) = sqlx::query_as(
        "SELECT revision,relative_path,item_id FROM catalog_files WHERE id=? AND generated=0",
    )
    .bind(file_id)
    .fetch_one(&mut **tx)
    .await?;
    let candidates = local_candidates(tx, file_id, &path, &item).await?;
    // An open proposal for an older file revision becomes stale first.
    let open: Option<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} WHERE file_id=? AND status IN ('pending','review','deferred')"
    ))
    .bind(file_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = open {
        let current = decode(row)?;
        if current.file_revision != revision {
            let stale = matching::refresh(&current, Some(&revision), current.candidates.clone())?;
            store(tx, &stale, false).await?;
        }
    }
    let latest: Option<Row> = sqlx::query_as(&format!(
        // The open proposal wins; otherwise insertion order. Proposals are never
        // deleted, so rowid is monotonic; wall-clock timestamps are not.
        "SELECT {COLUMNS} WHERE file_id=? ORDER BY status IN ('pending','review','deferred') DESC,rowid DESC LIMIT 1"
    ))
    .bind(file_id)
    .fetch_optional(&mut **tx)
    .await?;
    let latest = latest.map(decode).transpose()?;
    let proposal = match (
        matching::proposal_action(latest.as_ref(), &revision),
        latest,
    ) {
        // A decision for this exact file revision stands until explicitly reopened.
        (ProposalAction::Keep, Some(decided)) => decided,
        (ProposalAction::Refresh, Some(current)) => {
            let next = matching::refresh(&current, Some(&revision), candidates)?;
            if next != current {
                store(tx, &next, false).await?;
            }
            next
        }
        _ => {
            let fresh = matching::propose(new_id(), file_id.into(), revision, candidates)?;
            store(tx, &fresh, true).await?;
            fresh
        }
    };
    Ok(proposal)
}

#[derive(Debug, Serialize)]
pub struct Decided {
    pub proposal: Proposal,
    pub item_id: String,
    pub merge_receipt: Option<String>,
}

/// Apply an operator decision against the reviewed proposal revision.
pub async fn decide(
    app: &App,
    id: &str,
    expected_revision: u64,
    decision: Decision,
) -> Result<Decided, MatchingError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let decided = decide_in_transaction(&mut tx, id, expected_revision, decision).await?;
    tx.commit().await?;
    Ok(decided)
}

/// `decide` inside the caller's writer transaction; the caller holds
/// `App.jobs` and commits.
pub(crate) async fn decide_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    expected_revision: u64,
    decision: Decision,
) -> Result<Decided, MatchingError> {
    let row: Row = sqlx::query_as(&format!("SELECT {COLUMNS} WHERE id=?"))
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    let proposal = decode(row)?;
    let current: Option<(String, String)> =
        sqlx::query_as("SELECT revision,item_id FROM catalog_files WHERE id=?")
            .bind(&proposal.file_id)
            .fetch_optional(&mut **tx)
            .await?;
    let (next, identification) = matching::decide(
        &proposal,
        expected_revision,
        current.as_ref().map(|(r, _)| r.as_str()),
        decision,
    )?;
    let mut item = current.map(|(_, item)| item).unwrap_or_default();
    let mut merge_receipt = None;
    if let Some(identification) = identification {
        let candidate = identification.candidate;
        if let Some(target) = &candidate.item_id {
            let versions: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM media_versions v JOIN timelines t ON t.id=v.timeline_id JOIN editions e ON e.id=t.edition_id WHERE e.item_id=?",
            )
            .bind(&item)
            .fetch_one(&mut **tx)
            .await?;
            match matching::attachment(&item, target, usize::try_from(versions).unwrap_or(0)) {
                Attachment::AlreadyIdentified => {}
                Attachment::SplitRequired => return Err(MatchingError::SplitRequired),
                Attachment::MergeWork => {
                    let plan =
                        curation::plan_in_transaction(tx, vec![item.clone()], target.clone())
                            .await
                            .map_err(MatchingError::Curation)?;
                    let receipt = curation::merge_in_transaction(tx, &plan)
                        .await
                        .map_err(MatchingError::Curation)?;
                    merge_receipt = Some(receipt.id);
                    item = target.clone();
                }
            }
        }
        if let Some((namespace, value)) = &candidate.external_id {
            identify(tx, &item, namespace, value).await?;
        }
        sqlx::query("UPDATE items SET match_state='manual' WHERE id=?")
            .bind(&item)
            .execute(&mut **tx)
            .await?;
    }
    store(tx, &next, false).await?;
    Ok(Decided {
        proposal: next,
        item_id: item,
        merge_receipt,
    })
}

/// Record a provider identity on the work's contribution from that provider,
/// keeping any values that contribution already carries.
async fn identify(
    tx: &mut Transaction<'_, Sqlite>,
    item: &str,
    namespace: &str,
    value: &str,
) -> Result<(), MatchingError> {
    let source = namespace.split(':').next().unwrap_or(namespace);
    let empty = r#"{"values":{},"tags":[],"excluded_tags":[]}"#;
    let result = sqlx::query("INSERT INTO metadata_documents VALUES (?,?,1,?,?,?) ON CONFLICT(item_id,source) DO UPDATE SET external_id=excluded.external_id,revision=metadata_documents.revision+1,updated_at=excluded.updated_at WHERE metadata_documents.external_id IS NOT excluded.external_id")
        .bind(item).bind(source).bind(value).bind(empty).bind(now()).execute(&mut **tx).await;
    match result {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(MatchingError::IdentityTaken)
        }
        Err(e) => Err(e.into()),
        Ok(_) => Ok(()),
    }
}

/// Inside scan publication: open proposals whose file revision changed become
/// stale through the same core refresh decision.
pub(crate) async fn invalidate_changed(conn: &mut SqliteConnection) -> anyhow::Result<()> {
    let rows: Vec<(Row, String)> = sqlx::query_as::<_, (String, i64, String, String, String, String, Option<String>, String)>(
        "SELECT p.id,p.revision,p.file_id,p.file_revision,p.status,p.candidates_json,p.decision_json,f.revision FROM match_proposals p JOIN media_files f ON f.id=p.file_id WHERE p.status IN ('pending','review','deferred') AND f.revision<>p.file_revision",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(a, b, c, d, e, f, g, current)| ((a, b, c, d, e, f, g), current))
    .collect();
    for (row, current) in rows {
        let proposal = decode(row)?;
        let next = matching::refresh(&proposal, Some(&current), proposal.candidates.clone())
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        store(conn, &next, false).await?;
    }
    Ok(())
}

/// A manual identification pins the work's provider identity: a contribution
/// asserting a different identity for that provider, or withdrawing it, is refused.
pub(crate) async fn provider_identity_allowed(
    conn: &mut SqliteConnection,
    item: &str,
    source: &str,
    incoming: Option<&str>,
) -> anyhow::Result<bool> {
    let state: String = sqlx::query_scalar("SELECT match_state FROM items WHERE id=?")
        .bind(item)
        .fetch_one(&mut *conn)
        .await?;
    let state: MatchState = serde_json::from_value(serde_json::Value::String(state))?;
    let current: Option<Option<String>> = sqlx::query_scalar(
        "SELECT external_id FROM metadata_documents WHERE item_id=? AND source=?",
    )
    .bind(item)
    .bind(source)
    .fetch_optional(&mut *conn)
    .await?;
    let current = current.flatten().map(|v| (source.to_string(), v));
    let incoming = incoming.map(|v| (source.to_string(), v.to_string()));
    Ok(matching::provider_identity_allowed(
        state,
        current.as_ref(),
        incoming.as_ref(),
    ))
}
