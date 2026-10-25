//! Private persistence boundary. HTTP adapters retain current caller/native
//! scope proof and the writer through these operations and their receipt ACK.
use super::*;

/// Preserve the exact core access/CAS variant through anyhow transport.
#[derive(Debug)]
pub(crate) struct ScheduleAccessError(pub access::AccessError);
impl std::fmt::Display for ScheduleAccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "schedule access decision: {:?}", self.0)
    }
}
impl std::error::Error for ScheduleAccessError {}

pub(crate) async fn lookup_in(
    app: &App,
    db: &mut SqliteConnection,
    id: &str,
) -> anyhow::Result<Option<RetainedView>> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native schedule owner missing")?;
    let identity = runtime.identity().await?;
    if let Some((mut row, cursor, tombstoned)) = load_in(db, id).await? {
        let adopted = verify_origin_in(db, id, &identity).await?;
        row.next_run = cursor.retry_at;
        return Ok(Some(RetainedView {
            row,
            progress: None,
            adopted,
            native: true,
            tombstoned,
        }));
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_schedules WHERE id=?)")
        .bind(id)
        .fetch_one(&mut *db)
        .await?;
    if !exists {
        return Ok(None);
    }
    let (row, progress) = legacy_in(db, id).await?;
    Ok(Some(RetainedView {
        row,
        progress,
        adopted: false,
        native: false,
        tombstoned: false,
    }))
}
fn cursor(row: &RetainedSchedule) -> anyhow::Result<Cursor> {
    let plan = policy::plan_adoption(row.clone(), None).map_err(anyhow::Error::msg)?;
    match plan.disposition {
        AdoptionDisposition::ScanCandidate { cursor, .. } => Ok(cursor),
        _ => anyhow::bail!("native schedule task is not supported"),
    }
}
pub(crate) async fn create_in(
    app: &App,
    db: &mut SqliteConnection,
    row: RetainedSchedule,
) -> anyhow::Result<RetainedSchedule> {
    ensure!(row.revision == 1, "new schedule revision must be one");
    let cursor = cursor(&row)?;
    let collision:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_schedules WHERE id=? UNION ALL SELECT 1 FROM catabolic_scan_schedule_definitions WHERE id=? UNION ALL SELECT 1 FROM catabolic_scan_schedule_adoptions WHERE schedule_id=?)").bind(&row.id).bind(&row.id).bind(&row.id).fetch_one(&mut *db).await?;
    ensure!(!collision, "schedule identity already retained");
    principal_in(db, &row).await?;
    let identity = app
        .native_catalog
        .as_ref()
        .context("native schedule owner missing")?
        .identity()
        .await?;
    insert_definition_in(db, &identity, &row, &cursor, None).await?;
    hint_in(db, &row.id, "changed", Some(&row.library_id)).await?;
    Ok(row)
}
pub(crate) async fn replace_in(
    app: &App,
    db: &mut SqliteConnection,
    mut desired: RetainedSchedule,
    expected: u64,
) -> anyhow::Result<RetainedSchedule> {
    let view = lookup_in(app, db, &desired.id)
        .await?
        .context("native schedule not found")?;
    ensure!(
        view.native && !view.tombstoned,
        "schedule requires explicit adoption or is deleted"
    );
    let old = view.row;
    let changed = old.library_id != desired.library_id
        || old.task != desired.task
        || old.interval_seconds != desired.interval_seconds
        || old.enabled != desired.enabled;
    let revision = access::revise(u64::try_from(old.revision)?, expected, changed)
        .map_err(ScheduleAccessError)?;
    let access::Revised::Next(next) = revision else {
        return Ok(old);
    };
    desired.revision = i64::try_from(next)?;
    let cursor = cursor(&desired)?;
    principal_in(db, &desired).await?;
    let changed=sqlx::query("UPDATE catabolic_scan_schedule_definitions SET revision=?,library_id=?,enabled=?,definition_json=?,updated_at=? WHERE id=? AND revision=? AND tombstoned=0")
        .bind(desired.revision).bind(&desired.library_id).bind(desired.enabled).bind(serde_json::to_string(&desired)?).bind(crate::now()).bind(&desired.id).bind(old.revision).execute(&mut *db).await?.rows_affected();
    ensure!(changed == 1, "schedule definition CAS changed");
    let changed=sqlx::query("UPDATE catabolic_scan_schedule_cursors SET definition_revision=?,due_at=?,retry_at=? WHERE schedule_id=? AND definition_revision=?")
        .bind(desired.revision).bind(cursor.due_at).bind(cursor.retry_at).bind(&desired.id).bind(old.revision).execute(&mut *db).await?.rows_affected();
    ensure!(changed == 1, "schedule cursor CAS changed");
    hint_in(db, &desired.id, "changed", Some(&old.library_id)).await?;
    Ok(desired)
}
pub(crate) async fn tombstone_in(
    app: &App,
    db: &mut SqliteConnection,
    id: &str,
    expected: u64,
) -> anyhow::Result<()> {
    let view = lookup_in(app, db, id)
        .await?
        .context("native schedule not found")?;
    ensure!(
        view.native && !view.tombstoned,
        "schedule requires explicit adoption or is deleted"
    );
    let mut row = view.row;
    let old = row.revision;
    let access::Revised::Next(next) =
        access::revise(u64::try_from(old)?, expected, true).map_err(ScheduleAccessError)?
    else {
        unreachable!("deletion always changes definition")
    };
    row.revision = i64::try_from(next)?;
    row.enabled = false;
    let changed=sqlx::query("UPDATE catabolic_scan_schedule_definitions SET revision=?,enabled=0,tombstoned=1,definition_json=?,updated_at=? WHERE id=? AND revision=? AND tombstoned=0")
        .bind(row.revision).bind(serde_json::to_string(&row)?).bind(crate::now()).bind(id).bind(old).execute(&mut *db).await?.rows_affected();
    ensure!(changed == 1, "schedule deletion CAS changed");
    let changed=sqlx::query("UPDATE catabolic_scan_schedule_cursors SET definition_revision=? WHERE schedule_id=? AND definition_revision=?")
        .bind(row.revision).bind(id).bind(old).execute(&mut *db).await?.rows_affected();
    ensure!(changed == 1, "schedule cursor deletion CAS changed");
    hint_in(db, id, "deleted", Some(&row.library_id)).await?;
    Ok(())
}

/// Hints are committed with the definition mutation. They carry only prior
/// scope for safe reset; SSE independently resolves current ownership.
pub(super) async fn hint_in(
    db: &mut SqliteConnection,
    id: &str,
    kind: &str,
    prior: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO change_events(topic,resource_id,kind,scope_library) VALUES('schedules',?,?,?)",
    )
    .bind(id)
    .bind(kind)
    .bind(prior)
    .execute(db)
    .await?;
    Ok(())
}
