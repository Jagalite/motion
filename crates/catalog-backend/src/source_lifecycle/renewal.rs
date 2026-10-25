//! Complete merged91/94 history. Original baseline evidence is never rewritten.
use super::*;
const MAX_ROWS: i64 = 16_384;
const MAX_BYTES: i64 = 8 * 1024 * 1024;
const SELECT: &str = "SELECT epoch,catalog_id,incarnation,mapping_digest,source_revision,binding_revision,state,proof_json FROM catabolic_source_lifecycle WHERE source_id=? ORDER BY epoch";
const SIZE: &str = "SELECT count(*),coalesce(sum(length(CAST(catalog_id AS BLOB))+length(CAST(incarnation AS BLOB))+length(CAST(mapping_digest AS BLOB))+length(CAST(state AS BLOB))+length(CAST(proof_json AS BLOB))+length(CAST(operation_id AS BLOB))+24),0) FROM catabolic_source_lifecycle WHERE source_id=?";
const ACK_COUNT: &str = "SELECT count(*) FROM catabolic_source_relocation_acks ack JOIN catabolic_source_relocation_admissions intent USING(operation_id) WHERE intent.source_id=?";
const HISTORY_BYTES: &str = "SELECT coalesce(sum(length(CAST(i.operation_id AS BLOB))+length(CAST(i.source_id AS BLOB))+length(CAST(i.catalog_id AS BLOB))+length(CAST(i.incarnation AS BLOB))+length(CAST(i.predecessor_json AS BLOB))+length(CAST(i.old_mapping_json AS BLOB))+length(CAST(i.new_mapping_json AS BLOB))+length(CAST(i.old_mapping_digest AS BLOB))+length(CAST(i.new_mapping_digest AS BLOB))+length(CAST(i.requester_json AS BLOB))+length(CAST(i.commitment_json AS BLOB))+length(CAST(i.admission_digest AS BLOB))+coalesce(length(CAST(c.command_json AS BLOB))+length(CAST(c.command_digest AS BLOB)),0)+coalesce(length(CAST(a.native_effect_json AS BLOB))+length(CAST(a.native_effect_digest AS BLOB))+length(CAST(a.successor_json AS BLOB)),0)+24),0) FROM catabolic_source_relocation_admissions i LEFT JOIN catabolic_source_relocation_commands c USING(operation_id) LEFT JOIN catabolic_source_relocation_acks a USING(operation_id) WHERE i.source_id=?";
pub(super) async fn enabled_in(db: &mut SqliteConnection) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='catabolic_source_relocation_acks'").fetch_one(db).await?==1)
}
pub(super) fn enabled_sync(db: &rusqlite::Connection) -> anyhow::Result<bool> {
    Ok(db.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='catabolic_source_relocation_acks'",[],|r|r.get::<_,i64>(0))?==1)
}
fn bound(size: (i64, i64)) -> anyhow::Result<()> {
    ensure!(
        size.0 > 0 && size.0 <= MAX_ROWS && size.1 <= MAX_BYTES,
        "complete lifecycle history exceeds supported source bound"
    );
    Ok(())
}
fn next(
    id: &str,
    base: &Baseline,
    prior: &LifecycleProof,
    row: Row,
) -> anyhow::Result<LifecycleProof> {
    let (
        epoch,
        catalog_id,
        incarnation,
        mapping_digest,
        source_revision,
        binding_revision,
        state,
        proof,
    ) = row;
    ensure!(
        serde_json::from_str::<Baseline>(&proof)? == *base,
        "lifecycle baseline provenance changed"
    );
    let state: State = serde_json::from_value(json!(state))?;
    ensure!(
        prior.state != State::Tombstoned
            && prior.epoch.checked_add(1) == Some(epoch)
            && prior.source_revision.checked_add(1) == Some(source_revision)
            && catalog_id == prior.catalog_id
            && incarnation == prior.incarnation,
        "lifecycle predecessor chain differs"
    );
    if state != State::Active {
        ensure!(
            mapping_digest == prior.mapping_digest && binding_revision == prior.binding_revision,
            "withdrawal changed mapping without renewal"
        );
    }
    Ok(LifecycleProof {
        source_id: id.into(),
        epoch,
        catalog_id,
        incarnation,
        mapping_digest,
        source_revision,
        binding_revision,
        state,
    })
}
pub(super) async fn current_in(
    db: &mut SqliteConnection,
    id: &str,
    base: &Baseline,
) -> anyhow::Result<(LifecycleProof, SourceMapping)> {
    let size: (i64, i64) = sqlx::query_as(SIZE).bind(id).fetch_one(&mut *db).await?;
    let extra: i64 = sqlx::query_scalar(HISTORY_BYTES)
        .bind(id)
        .fetch_one(&mut *db)
        .await?;
    bound((
        size.0,
        size.1
            .checked_add(extra)
            .context("lifecycle byte overflow")?,
    ))?;
    let rows: Vec<Row> = sqlx::query_as(SELECT).bind(id).fetch_all(&mut *db).await?;
    let renewals = rows.iter().filter(|r| r.0 > 0 && r.6 == "active").count() as i64;
    let mut rows = rows.into_iter();
    let first = rows.next().context("lifecycle baseline absent")?;
    ensure!(first.0 == 0, "lifecycle history missing initial epoch");
    let mut current = resolve(id, base, first)?;
    let mut mapping = base.mapping.clone();
    for row in rows {
        let successor = next(id, base, &current, row)?;
        if successor.state == State::Active {
            let operation: String = sqlx::query_scalar(
                "SELECT operation_id FROM catabolic_source_lifecycle WHERE source_id=? AND epoch=?",
            )
            .bind(id)
            .bind(successor.epoch)
            .fetch_one(&mut *db)
            .await?;
            mapping = crate::source_relocations::history::mapping_in(
                db, &operation, &current, &mapping, &successor,
            )
            .await?;
        }
        current = successor;
    }
    ensure!(
        sqlx::query_scalar::<_, i64>(ACK_COUNT)
            .bind(id)
            .fetch_one(db)
            .await?
            == renewals,
        "orphan or duplicate relocation ACK"
    );
    Ok((current, mapping))
}
pub(super) fn current_sync(
    db: &rusqlite::Connection,
    id: &str,
    base: &Baseline,
) -> anyhow::Result<(LifecycleProof, SourceMapping)> {
    let size: (i64, i64) = db.query_row(SIZE, [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let extra: i64 = db.query_row(HISTORY_BYTES, [id], |r| r.get(0))?;
    bound((
        size.0,
        size.1
            .checked_add(extra)
            .context("lifecycle byte overflow")?,
    ))?;
    let mut stmt = db.prepare(SELECT)?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<Result<Vec<Row>, _>>()?;
    let renewals = rows.iter().filter(|r| r.0 > 0 && r.6 == "active").count() as i64;
    let mut rows = rows.into_iter();
    let first = rows.next().context("lifecycle baseline absent")?;
    ensure!(first.0 == 0, "lifecycle history missing initial epoch");
    let mut current = resolve(id, base, first)?;
    let mut mapping = base.mapping.clone();
    for row in rows {
        let successor = next(id, base, &current, row)?;
        if successor.state == State::Active {
            let operation: String = db.query_row(
                "SELECT operation_id FROM catabolic_source_lifecycle WHERE source_id=? AND epoch=?",
                rusqlite::params![id, successor.epoch],
                |r| r.get(0),
            )?;
            mapping = crate::source_relocations::history::mapping_sync(
                db, &operation, &current, &mapping, &successor,
            )?;
        }
        current = successor;
    }
    ensure!(
        db.query_row(ACK_COUNT, [id], |r| r.get::<_, i64>(0))? == renewals,
        "orphan or duplicate relocation ACK"
    );
    Ok((current, mapping))
}
