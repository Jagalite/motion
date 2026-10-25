//! Native scan-only consumer. Never calls legacy maintenance, search,
//! preview, metadata fanout or legacy scan request owners.
use super::*;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
pub(crate) struct TickReport {
    pub admitted: Vec<String>,
    pub capacity_blocked: usize,
    pub refused: Vec<(String, String)>,
    pub retained_legacy_pending: i64,
    pub actorless_v1_pending: i64,
    pub cancelled: bool,
}
/// One bounded pass. Historical adoption is explicit, never inferred from a
/// missing companion. Refused rows retain their exact due occurrence and actor;
/// a later policy repair can retry without inventing a new occurrence.
#[derive(Debug, Default)]
pub(crate) struct Sweep {
    after: Option<(i64, String)>,
}

async fn due_page(
    db: &mut SqliteConnection,
    now: i64,
    sweep: &Sweep,
) -> anyhow::Result<Vec<(i64, String)>> {
    let (after_time, after_id) = sweep
        .after
        .as_ref()
        .map(|(time, id)| (*time, id.as_str()))
        .unwrap_or((i64::MIN, ""));
    Ok(sqlx::query_as("SELECT c.retry_at,d.id FROM catabolic_scan_schedule_definitions d JOIN catabolic_scan_schedule_cursors c ON c.schedule_id=d.id WHERE d.enabled=1 AND d.tombstoned=0 AND c.retry_at<=? AND (c.retry_at>? OR (c.retry_at=? AND d.id>?)) ORDER BY c.retry_at,d.id LIMIT 100")
        .bind(now).bind(after_time).bind(after_time).bind(after_id).fetch_all(db).await?)
}

pub(crate) async fn tick_once(
    app: &App,
    stop: &CancellationToken,
    now: i64,
) -> anyhow::Result<TickReport> {
    tick_sweep(app, stop, now, &mut Sweep::default()).await
}

pub(crate) async fn tick_sweep(
    app: &App,
    stop: &CancellationToken,
    now: i64,
    sweep: &mut Sweep,
) -> anyhow::Result<TickReport> {
    tick_sweep_inner(
        app,
        stop,
        now,
        sweep,
        #[cfg(test)]
        None,
    )
    .await
}
async fn tick_sweep_inner(
    app: &App,
    stop: &CancellationToken,
    now: i64,
    sweep: &mut Sweep,
    #[cfg(test)] after_read: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
) -> anyhow::Result<TickReport> {
    ensure!(
        app.native_catalog.is_some(),
        "native schedule owner missing"
    );
    let mut report = TickReport::default();
    let mut tx = app.db.begin().await?;
    #[cfg(test)]
    if let Some((entered, release)) = after_read {
        let _ = entered.send(());
        release
            .await
            .context("schedule test read barrier dropped")?;
    }
    // Never treat an unverified adoption link alone as completed migration.
    report.retained_legacy_pending = i64::try_from(
        retained_inventory_in(app, &mut tx)
            .await?
            .iter()
            .filter(|row| !row.native)
            .count(),
    )?;
    report.actorless_v1_pending = sqlx::query_scalar("SELECT count(*) FROM scan_schedules")
        .fetch_one(&mut *tx)
        .await?;
    let ids = due_page(&mut tx, now, sweep).await?;
    let exhausted = ids.len() < 100;
    tx.commit().await?;
    for (retry_at, id) in ids {
        if stop.is_cancelled() {
            report.cancelled = true;
            break;
        }
        sweep.after = Some((retry_at, id.clone()));
        // Await the current admission transaction to its actual outcome. Do not
        // drop it on shutdown and claim that committed demand was cancelled.
        match fire_one(app, &id, now).await {
            Ok(Fire::Admitted(request)) => report.admitted.push(request),
            Ok(Fire::CapacityBlocked) => report.capacity_blocked += 1,
            Ok(Fire::Idle) => {}
            Err(error) => report.refused.push((id, format!("{error:#}"))),
        }
    }
    if exhausted && !report.cancelled {
        sweep.after = None;
    }
    Ok(report)
}
/// Service-owned timer: cancellation joins any active admission before exit.
pub(crate) async fn worker(app: App, stop: CancellationToken) -> anyhow::Result<()> {
    let mut sweep = Sweep::default();
    loop {
        tokio::select! {
            _=stop.cancelled()=>return Ok(()),
            _=tokio::time::sleep(std::time::Duration::from_secs(30))=>{}
        }
        let report = match tick_sweep(&app, &stop, crate::now(), &mut sweep).await {
            Ok(report) => report,
            Err(error) if cancelled_before_admission(&stop, &error) => return Ok(()),
            Err(error) => return Err(error),
        };
        for (id, error) in report.refused {
            tracing::warn!(schedule_id=%id,%error,"native schedule admission retained for reconciliation");
        }
        if report.retained_legacy_pending > 0 || report.actorless_v1_pending > 0 {
            tracing::warn!(
                retained_legacy_pending = report.retained_legacy_pending,
                actorless_v1_pending = report.actorless_v1_pending,
                "native schedule adoption obligations remain"
            );
        }
        if report.cancelled {
            return Ok(());
        }
    }
}

#[cfg(test)]
pub(super) async fn tick_after_read_barrier(
    app: &App,
    stop: &CancellationToken,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
) -> anyhow::Result<TickReport> {
    tick_sweep_inner(
        app,
        stop,
        crate::now(),
        &mut Sweep::default(),
        Some((entered, release)),
    )
    .await
}

// Only the exact native pre-effect drain refusal is normal shutdown. SQL,
// inventory, authority and post-effect errors are never hidden by cancellation.
pub(super) fn cancelled_before_admission(stop: &CancellationToken, error: &anyhow::Error) -> bool {
    stop.is_cancelled()
        && error
            .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_accepts_only_typed_pre_effect_drain_refusal() {
        let stop = CancellationToken::new();
        let refused = anyhow::Error::new(crate::native_catalog::OrdinaryAdmissionDraining)
            .context("schedule inventory");
        assert!(!cancelled_before_admission(&stop, &refused));
        stop.cancel();
        assert!(cancelled_before_admission(&stop, &refused));
        for error in [
            anyhow::anyhow!("native ordinary admission is draining"),
            anyhow::anyhow!("inventory mismatch"),
            anyhow::Error::new(sqlx::Error::PoolClosed),
        ] {
            assert!(!cancelled_before_admission(&stop, &error));
        }
    }

    /// Storage/keyset test only: no synthetic row is used as an authorization
    /// grant or passed to fire_one. Simulate refusal without changing due rows.
    #[tokio::test]
    async fn refused_prefix_does_not_starve_later_due_rows() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/0083_catabolic_scan_demands.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/0092_catabolic_scan_schedules.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let mut db = pool.acquire().await.unwrap();
        let identity =
            serde_json::json!({"catalog_id":"catalog","catalog_incarnation":"incarnation"});
        for index in 0..102 {
            let row = RetainedSchedule {
                id: format!("schedule-{index:03}"),
                revision: 1,
                library_id: "library".into(),
                task: "scan".into(),
                interval_seconds: 60,
                enabled: true,
                next_run: 100,
                requester_id: "operator".into(),
                credential_hash: None,
                ingress_id: None,
            };
            let cursor = Cursor {
                definition_revision: 1,
                due_at: 100,
                retry_at: 100,
            };
            insert_definition_in(&mut db, &identity, &row, &cursor, None)
                .await
                .unwrap();
        }
        let mut sweep = Sweep::default();
        let first = due_page(&mut db, 100, &sweep).await.unwrap();
        assert_eq!(first.len(), 100);
        // Every observed row advances fairness even if admission refuses it.
        for key in first {
            sweep.after = Some(key);
        }
        let second = due_page(&mut db, 100, &sweep).await.unwrap();
        assert_eq!(
            second,
            vec![(100, "schedule-100".into()), (100, "schedule-101".into())]
        );
        for key in &second {
            sweep.after = Some(key.clone());
        }
        assert!(due_page(&mut db, 100, &sweep).await.unwrap().is_empty());
        // Reset/restart repeats the original due identities; fairness never
        // edits the durable cursor or claims that any request was admitted.
        let reset = due_page(&mut db, 100, &Sweep::default()).await.unwrap();
        assert_eq!(reset[0], (100, "schedule-000".into()));
        let unchanged: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_scan_schedule_cursors WHERE definition_revision=1 AND due_at=100 AND retry_at=100")
            .fetch_one(&mut *db).await.unwrap();
        assert_eq!(unchanged, 102);
    }
}
