//! Bounded, best-effort operational observations. These samples never decide
//! admission, publication, retries, or any other application correctness rule.
//! SQLite, timers, and filesystem observation consequently stay in this adapter.
use crate::App;
use serde::{Deserialize, Serialize};
use sqlx::{
    QueryBuilder, Sqlite, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio_util::sync::CancellationToken;

pub const INTERVAL_SECONDS: u64 = 10;
pub const RETENTION_SECONDS: i64 = 86_400;
pub const MAX_POINTS: usize = 2_000;
// One ordinary day has 8,640 samples. Bound SQL's window work as well as its
// response even if pathological restart churn creates more observations.
const MAX_READ_SAMPLES: i64 = 12_000;
const STORAGE_INTERVAL_SECONDS: u64 = 60;
const PRUNE_LIMIT: i64 = 4_096;
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(3);

const SERIES: [(&str, &str, &str); 18] = [
    ("process.cpu_seconds_total", "counter", "seconds"),
    ("process.resident_bytes", "gauge", "bytes"),
    ("process.open_files", "gauge", "count"),
    ("http.requests_total", "counter", "count"),
    ("http.request_errors_total", "counter", "count"),
    ("http.requests_in_flight", "gauge", "count"),
    ("deliveries.active", "gauge", "count"),
    ("jobs.queued", "gauge", "count"),
    ("jobs.running", "gauge", "count"),
    ("jobs.failed_total", "counter", "count"),
    ("db.busy_total", "counter", "count"),
    ("storage.database_bytes", "gauge", "bytes"),
    ("storage.asset_bytes", "gauge", "bytes"),
    ("storage.temporary_bytes", "gauge", "bytes"),
    ("storage.free_bytes", "gauge", "bytes"),
    ("playback.sessions_active", "gauge", "count"),
    ("playback.bytes_sent_total", "counter", "bytes"),
    ("playback.transcodes_active", "gauge", "count"),
];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    pub since: Option<String>,
    pub step_seconds: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ReadWindow {
    since_millis: i64,
    step_millis: i64,
}

impl HistoryQuery {
    /// Clamp the requested history to retention, and increase resolution only
    /// as needed for the response bound. Timestamps are parsed before clamping:
    /// malformed dates cannot silently turn into the default retention window.
    pub fn validate(&self, now: i64) -> Result<ReadWindow, &'static str> {
        let floor = now.saturating_sub(RETENTION_SECONDS).saturating_mul(1_000);
        let since_millis = match &self.since {
            Some(value) => {
                // time accepts the human-readable space separator too; this
                // endpoint deliberately requires RFC3339's T/t separator.
                if !matches!(value.as_bytes().get(10), Some(b'T' | b't')) {
                    return Err("since must be an RFC 3339 timestamp");
                }
                let parsed = OffsetDateTime::parse(value, &Rfc3339)
                    .map_err(|_| "since must be an RFC 3339 timestamp")?;
                // Round up, so a fractional since never includes an earlier
                // millisecond sample. RFC3339's supported years fit in i64 ms.
                let nanos = parsed.unix_timestamp_nanos();
                let millis =
                    nanos.div_euclid(1_000_000) + i128::from(nanos.rem_euclid(1_000_000) != 0);
                i64::try_from(millis)
                    .map_err(|_| "since is outside the supported timestamp range")?
                    .max(floor)
            }
            None => floor,
        };
        let requested = match &self.step_seconds {
            Some(value) if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => value
                .parse::<u64>()
                .map_err(|_| "step_seconds must be an integer of at least 10")?,
            Some(_) => return Err("step_seconds must be an integer of at least 10"),
            None => INTERVAL_SECONDS,
        };
        if requested < INTERVAL_SECONDS {
            return Err("step_seconds must be an integer of at least 10");
        }
        // Reserve endpoint buckets by dividing by MAX_POINTS - 1. Very large
        // steps cover the entire supported calendar without overflowing SQL.
        let span = now
            .saturating_mul(1_000)
            .saturating_sub(since_millis)
            .max(0) as u64;
        let automatic = span.div_ceil((MAX_POINTS - 1) as u64).div_ceil(10_000) * 10_000;
        let step_millis = requested
            .saturating_mul(1_000)
            .max(automatic)
            .min(i64::MAX as u64) as i64;
        Ok(ReadWindow {
            since_millis,
            step_millis,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct MetricPoint {
    pub t: String,
    pub v: String,
}

#[derive(Debug, Serialize)]
pub struct MetricSeries {
    pub id: &'static str,
    pub kind: &'static str,
    pub unit: &'static str,
    pub points: Vec<MetricPoint>,
}

#[derive(Debug, Serialize)]
pub struct MetricsHistory {
    pub sampled_at: String,
    pub interval_seconds: u64,
    pub retention_seconds: i64,
    pub series: Vec<MetricSeries>,
}

fn millis_timestamp(millis: i64) -> String {
    let mut result = crate::v2::timestamp(millis.div_euclid(1_000));
    let fraction = millis.rem_euclid(1_000);
    if fraction != 0 {
        result.pop();
        result.push_str(&format!(".{fraction:03}Z"));
    }
    result
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// Isolate this adapter from the job pool and its five-second busy timeout.
/// WAL reads use separate connections and never acquire the application writer
/// mutex. A contended sample fails immediately instead of joining its queue.
pub async fn pool(app: &App) -> Result<SqlitePool, sqlx::Error> {
    app.health
        .metrics
        .history
        .get_or_try_init(|| writer_pool(app.db.connect_options().as_ref().clone()))
        .await
        .cloned()
}

/// The sampler's pool over `database`: a busy writer or a full pool fails
/// the sample at once (it is logged and the next interval retries) rather
/// than waiting behind application writes.
async fn writer_pool(database: SqliteConnectOptions) -> Result<SqlitePool, sqlx::Error> {
    SqlitePoolOptions::new()
        .after_connect(|connection, _| {
            Box::pin(crate::catalog_backend::projection::configure_connection(
                connection,
            ))
        })
        .max_connections(3)
        .min_connections(0)
        .acquire_timeout(Duration::from_millis(100))
        .connect_with(database.busy_timeout(Duration::ZERO))
        .await
}

pub async fn read(app: &App, window: ReadWindow, now: i64) -> Result<MetricsHistory, sqlx::Error> {
    read_pool(&pool(app).await?, window, now).await
}

/// Reads are bounded both by the retention predicate and by a per-series LIMIT.
/// Within each run and epoch-anchored bucket, choose the actual latest sample.
/// Counters also retain the first observed value of each run: merging across a
/// restart would hide its reset. Pathological restart counts truncate oldest
/// selected points to retain the hard response cap.
pub async fn read_pool(
    pool: &SqlitePool,
    window: ReadWindow,
    now: i64,
) -> Result<MetricsHistory, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut series = Vec::with_capacity(SERIES.len());
    let through = now.saturating_mul(1_000).saturating_add(999);
    let latest: Option<i64> = sqlx::query_scalar(
        "SELECT max(sampled_at) FROM operational_metrics WHERE sampled_at>=? AND sampled_at<=?",
    )
    .bind(now.saturating_sub(RETENTION_SECONDS).saturating_mul(1_000))
    .bind(through)
    .fetch_one(&mut *tx)
    .await?;
    for (id, kind, unit) in SERIES {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "WITH recent AS (
                SELECT id,sampled_at,value,run_id FROM operational_metrics
                WHERE series_id=? AND sampled_at>=? AND sampled_at<=?
                ORDER BY sampled_at DESC,id DESC LIMIT ?
            ), ranked AS (
                SELECT id,sampled_at,value,
                    -- Counters keep the last sample per bucket within each
                    -- run, so a restart's reset stays visible; gauges keep
                    -- only the last sample of each bucket.
                    row_number() OVER (PARTITION BY CASE WHEN ? THEN run_id END,sampled_at/? ORDER BY sampled_at DESC,id DESC) AS bucket_rank,
                    row_number() OVER (PARTITION BY run_id ORDER BY sampled_at,id) AS run_rank
                FROM recent
            ), bounded AS (
                SELECT id,sampled_at,value FROM ranked
                WHERE bucket_rank=1 OR (? AND run_rank=1)
                ORDER BY sampled_at DESC,id DESC LIMIT ?
            ) SELECT sampled_at,value FROM bounded ORDER BY sampled_at,id",
        )
        .bind(id)
        .bind(window.since_millis)
        .bind(through)
        .bind(MAX_READ_SAMPLES)
        .bind(kind == "counter")
        .bind(window.step_millis)
        .bind(kind == "counter")
        .bind(MAX_POINTS as i64)
        .fetch_all(&mut *tx)
        .await?;
        series.push(MetricSeries {
            id,
            kind,
            unit,
            points: rows
                .into_iter()
                .map(|(t, v)| MetricPoint {
                    t: millis_timestamp(t),
                    v,
                })
                .collect(),
        });
    }
    tx.commit().await?;
    Ok(MetricsHistory {
        sampled_at: latest
            .map(millis_timestamp)
            .unwrap_or_else(|| crate::v2::timestamp(now)),
        interval_seconds: INTERVAL_SECONDS,
        retention_seconds: RETENTION_SECONDS,
        series,
    })
}

/// This tiny fail-fast transaction is the only sampling write. No subprocess,
/// process inspection, directory walk, or source-table scan holds its lock.
pub async fn record(
    pool: &SqlitePool,
    run_id: &str,
    at: i64,
    values: &[(&str, String)],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    insert(&mut tx, run_id, at, values).await?;
    tx.commit().await
}

async fn insert(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    run_id: &str,
    at: i64,
    values: &[(&str, String)],
) -> Result<(), sqlx::Error> {
    if values.is_empty() {
        return Ok(());
    }
    let mut query = QueryBuilder::<Sqlite>::new(
        "INSERT OR IGNORE INTO operational_metrics(series_id,sampled_at,run_id,value) ",
    );
    query.push_values(values, |mut row, (id, value)| {
        row.push_bind(*id)
            .push_bind(at)
            .push_bind(run_id)
            .push_bind(value);
    });
    query.build().execute(&mut **tx).await?;
    Ok(())
}

/// Bounded deletion makes progress faster than the maximum normal insert rate.
/// Readers independently enforce retention even while an old backlog is pruned.
pub async fn prune(pool: &SqlitePool, run_id: &str, now: i64) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("DELETE FROM operational_metrics WHERE id IN (SELECT id FROM operational_metrics WHERE sampled_at<? ORDER BY sampled_at,id LIMIT ?)")
        .bind(now.saturating_sub(RETENTION_SECONDS).saturating_mul(1_000))
        .bind(PRUNE_LIMIT).execute(&mut *tx).await?;
    // Two index ranges avoid scanning all keys of a long-running current run.
    sqlx::query("DELETE FROM operational_metric_failures WHERE rowid IN (SELECT rowid FROM operational_metric_failures WHERE run_id<? UNION ALL SELECT rowid FROM operational_metric_failures WHERE run_id>? LIMIT ?)")
        .bind(run_id).bind(run_id).bind(PRUNE_LIMIT).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM operational_metric_counters WHERE run_id IN (SELECT run_id FROM operational_metric_counters WHERE run_id<? UNION ALL SELECT run_id FROM operational_metric_counters WHERE run_id>? LIMIT ?)")
        .bind(run_id).bind(run_id).bind(PRUNE_LIMIT).execute(&mut *tx).await?;
    tx.commit().await
}

/// The sampler's own writes, one at a time. They share the fail-fast pool, so
/// without this the sampler, its storage sampler and retention pruning would
/// make one another skip a sample at startup. Any other writer still makes a
/// sample fail at once (`writer_pool`).
#[derive(Clone, Default)]
struct Writes(std::sync::Arc<tokio::sync::Mutex<()>>);

async fn sample(app: &App, pool: &SqlitePool, run_id: &str, writes: &Writes) -> anyhow::Result<()> {
    let process = app
        .health
        .metrics
        .snapshot()
        .await
        .map_err(anyhow::Error::msg)?;
    let mut tx = pool.begin().await?;
    let (queued,running):(i64,i64)=sqlx::query_as("SELECT coalesce(sum(phase='queued'),0),coalesce(sum(phase IN ('admitted','running','validating','publishing','cancelling')),0) FROM (SELECT phase FROM jobs UNION ALL SELECT phase FROM processing_jobs)")
        .fetch_one(&mut *tx).await?;
    let started: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operational_metric_counters WHERE run_id=?)",
    )
    .bind(run_id)
    .fetch_one(&mut *tx)
    .await?;
    // The first persisted sample records zero jobs observed in this runtime
    // before beginning failure observation, making its real reset visible.
    // Thereafter use the same bounded sources as diagnostics and count every
    // identity once, even when old failures are later removed.
    let failures: Vec<(String, String)> = if started {
        sqlx::query_as("SELECT source,id FROM (SELECT 'scan' AS source,id,created_at AS changed FROM jobs WHERE phase='failed' UNION ALL SELECT 'processing',id,updated_at FROM processing_jobs WHERE phase='failed') ORDER BY changed DESC,id LIMIT 100")
            .fetch_all(&mut *tx).await?
    } else {
        vec![]
    };
    tx.commit().await?;
    let at = now_millis();
    let playback = app.processing.deliveries.playback_counts();
    let mut values = vec![
        (
            "process.cpu_seconds_total",
            process.system.cpu_seconds_total,
        ),
        ("process.resident_bytes", process.system.resident_bytes),
        ("http.requests_total", process.requests_total),
        ("http.request_errors_total", process.request_errors_total),
        ("http.requests_in_flight", process.requests_in_flight),
        (
            "deliveries.active",
            app.processing.deliveries.active_sessions().to_string(),
        ),
        ("jobs.queued", queued.to_string()),
        ("jobs.running", running.to_string()),
        ("playback.sessions_active", playback.0.to_string()),
        (
            "playback.bytes_sent_total",
            app.processing
                .deliveries
                .bytes_sent
                .load(Ordering::Relaxed)
                .to_string(),
        ),
        ("playback.transcodes_active", playback.1.to_string()),
        (
            "db.busy_total",
            crate::v2::system::DB_BUSY_COUNT
                .load(Ordering::Relaxed)
                .to_string(),
        ),
    ];
    if let Some(value) = process.system.open_files {
        values.push(("process.open_files", value));
    }
    let _turn = writes.0.lock().await;
    record_observations(pool, run_id, at, values, &failures).await
}

async fn record_observations(
    pool: &SqlitePool,
    run_id: &str,
    at: i64,
    mut values: Vec<(&str, String)>,
    failures: &[(String, String)],
) -> anyhow::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let previous: Option<String> =
        sqlx::query_scalar("SELECT failed_total FROM operational_metric_counters WHERE run_id=?")
            .bind(run_id)
            .fetch_optional(&mut *tx)
            .await?;
    let previous = previous
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(0);
    let mut added = 0;
    if !failures.is_empty() {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO operational_metric_failures(run_id,source,job_id) ",
        );
        query.push_values(failures, |mut row, (source, id)| {
            row.push_bind(run_id).push_bind(source).push_bind(id);
        });
        added = query.build().execute(&mut *tx).await?.rows_affected();
    }
    let next_failed = previous.saturating_add(added);
    sqlx::query("INSERT INTO operational_metric_counters(run_id,failed_total) VALUES(?,?) ON CONFLICT(run_id) DO UPDATE SET failed_total=excluded.failed_total")
        .bind(run_id).bind(next_failed.to_string()).execute(&mut *tx).await?;
    values.push(("jobs.failed_total", next_failed.to_string()));
    insert(&mut tx, run_id, at, &values).await?;
    tx.commit().await?;
    Ok(())
}

async fn storage_sample(
    app: &App,
    pool: &SqlitePool,
    run_id: &str,
    writes: &Writes,
    stop: &CancellationToken,
) -> anyhow::Result<()> {
    let (database_bytes, asset_bytes, generated) = tokio::time::timeout(SAMPLE_TIMEOUT, async {
        let mut tx = pool.begin().await?;
        let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
            .fetch_one(&mut *tx)
            .await?;
        let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
            .fetch_one(&mut *tx)
            .await?;
        let assets: i64 =
            sqlx::query_scalar("SELECT coalesce(sum(length(bytes)),0) FROM artwork_assets")
                .fetch_one(&mut *tx)
                .await?;
        let generated: i64 = sqlx::query_scalar(
            "SELECT coalesce(sum(bytes),0) FROM media_files WHERE generated=1 AND available=1",
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok::<_, sqlx::Error>((
            (pages.max(0) as u64).saturating_mul(page_size.max(0) as u64),
            (assets.max(0) as u64).saturating_add(generated.max(0) as u64),
            generated.max(0) as u64,
        ))
    })
    .await??;
    // Release the database connection before walking. The independent task
    // allows normal 10-second sampling and history requests to keep progressing.
    let cache = app.processing.root.clone();
    let root = app.storage.root.clone();
    let cancel = stop.clone();
    let (cache_bytes, free) = tokio::task::spawn_blocking(move || {
        let bytes = bounded_cache_bytes(cache, &cancel)?;
        Ok::<_, anyhow::Error>((bytes, fs2::available_space(root).ok()))
    })
    .await??;
    let mut values = vec![
        ("storage.database_bytes", database_bytes.to_string()),
        ("storage.asset_bytes", asset_bytes.to_string()),
        (
            "storage.temporary_bytes",
            cache_bytes.saturating_sub(generated).to_string(),
        ),
    ];
    if let Some(value) = free {
        values.push(("storage.free_bytes", value.to_string()));
    }
    let _turn = writes.0.lock().await;
    record(pool, run_id, now_millis(), &values).await?;
    Ok(())
}

fn bounded_cache_bytes(root: PathBuf, stop: &CancellationToken) -> anyhow::Result<u64> {
    match std::fs::metadata(&root) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    }
    let started = std::time::Instant::now();
    let mut total = 0u64;
    for (index, entry) in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .enumerate()
    {
        anyhow::ensure!(
            !stop.is_cancelled()
                && index < 1_000_000
                && started.elapsed() < Duration::from_secs(30),
            "metrics_storage_budget"
        );
        let entry = entry?;
        anyhow::ensure!(!entry.file_type().is_symlink(), "metrics_storage_symlink");
        if entry.file_type().is_file() {
            total = total
                .checked_add(entry.metadata()?.len())
                .ok_or_else(|| anyhow::anyhow!("metrics_storage_overflow"))?;
        }
    }
    Ok(total)
}

async fn storage_worker(
    app: App,
    pool: SqlitePool,
    run_id: String,
    writes: Writes,
    stop: CancellationToken,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(STORAGE_INTERVAL_SECONDS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            _ = tick.tick() => {},
        }
        // storage_sample retains an owned blocking filesystem operation. Do
        // not select-drop it on cancellation: its walker observes stop and the
        // actual JoinHandle must finish before this sampler can be joined.
        if storage_sample(&app, &pool, &run_id, &writes, &stop)
            .await
            .is_err()
        {
            tracing::warn!(
                code = "metrics_storage_sample_skipped",
                "Operational storage sample skipped"
            );
        }
    }
}

/// Spawned independently of the supervised application worker. Failure affects
/// observation only. Initialization is retried without changing server health.
pub async fn worker(app: App, stop: CancellationToken) -> anyhow::Result<()> {
    let run_id = crate::new_id();
    let writes = Writes::default();
    let mut tick = tokio::time::interval(Duration::from_secs(INTERVAL_SECONDS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_prune = None;
    let mut storage: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            _ = tick.tick() => {},
        }
        let pool = match pool(&app).await {
            Ok(pool) => pool,
            Err(_) => {
                tracing::warn!(
                    code = "metrics_store_unavailable",
                    "Operational metrics store unavailable"
                );
                continue;
            }
        };
        if storage.as_ref().is_some_and(|task| task.is_finished()) {
            if let Some(task) = storage.take() {
                // A panic/abort may have detached blocking work. Preserve the
                // failure rather than claiming a clean stopped-service join.
                task.await?;
            }
            tracing::warn!(
                code = "metrics_storage_worker_stopped",
                "Operational storage sampler stopped unexpectedly; restarting"
            );
        }
        if storage.is_none() {
            storage = Some(tokio::spawn(storage_worker(
                app.clone(),
                pool.clone(),
                run_id.clone(),
                writes.clone(),
                stop.clone(),
            )));
        }
        let result = tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            result = tokio::time::timeout(SAMPLE_TIMEOUT, sample(&app,&pool,&run_id,&writes)) => result,
        };
        if !matches!(result, Ok(Ok(()))) {
            tracing::warn!(
                code = "metrics_sample_skipped",
                "Operational metrics sample skipped"
            );
        }
        let now = crate::now();
        if last_prune.is_none_or(|last| now >= last + STORAGE_INTERVAL_SECONDS as i64) {
            if !matches!(
                tokio::time::timeout(SAMPLE_TIMEOUT, async {
                    let _turn = writes.0.lock().await;
                    prune(&pool, &run_id, now).await
                })
                .await,
                Ok(Ok(()))
            ) {
                tracing::warn!(
                    code = "metrics_prune_skipped",
                    "Operational metrics retention pass skipped"
                );
            }
            last_prune = Some(now);
        }
    }
    if let Some(task) = storage {
        // stop is cancelled; retain and join its real blocking cleanup.
        task.await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteJournalMode;

    /// A patient pool for arranging and inspecting test state. Closing a pool
    /// can return before its last connection's close (and WAL checkpoint)
    /// finishes, so a pool opened right after may briefly see the file
    /// locked: that is test lifecycle, not the contention production fails
    /// fast on. Contention is asserted only through `writer_pool`.
    async fn database(path: &std::path::Path) -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .busy_timeout(Duration::from_secs(5)),
            )
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/0042_operational_metrics.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    /// Reopen an existing test database patiently (see `database`).
    async fn reopen(path: &std::path::Path) -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .busy_timeout(Duration::from_secs(5)),
            )
            .await
            .unwrap()
    }

    #[test]
    fn query_validation_and_fractional_bounds() {
        let now = 1_800_000_000;
        let default = HistoryQuery::default().validate(now).unwrap();
        assert_eq!(default.since_millis, (now - RETENTION_SECONDS) * 1_000);
        assert_eq!(default.step_millis, 50_000);
        for since in [
            "",
            "tomorrow",
            "2026-02-30T00:00:00Z",
            "2026-10-12",
            "2026-10-12T00:00:00",
            "2026-10-12 00:00:00Z",
        ] {
            assert!(
                HistoryQuery {
                    since: Some(since.into()),
                    step_seconds: None
                }
                .validate(now)
                .is_err(),
                "{since}"
            );
        }
        for step in ["", "0", "9", "-10", "10.0", "+10", "18446744073709551616"] {
            assert!(
                HistoryQuery {
                    since: None,
                    step_seconds: Some(step.into())
                }
                .validate(now)
                .is_err(),
                "{step}"
            );
        }
        let query = HistoryQuery {
            since: Some("2027-01-15T08:00:00.000000001+00:00".into()),
            step_seconds: Some("10".into()),
        };
        assert_eq!(query.validate(now).unwrap().since_millis, 1_800_000_000_001);
        assert!(
            HistoryQuery {
                since: None,
                step_seconds: Some(u64::MAX.to_string())
            }
            .validate(now)
            .is_ok()
        );
    }

    #[tokio::test]
    async fn last_per_bucket_retains_counter_restart_and_all_series() {
        let dir = tempfile::tempdir().unwrap();
        let pool = database(&dir.path().join("history.sqlite3")).await;
        let now = 1_800_000_030;
        let base = (now - 30) * 1_000;
        for (run, offset, value) in [
            ("old", 0, "80"),
            ("old", 10_000, "90"),
            ("new", 11_000, "0"),
            ("new", 20_000, "3"),
        ] {
            record(
                &pool,
                run,
                base + offset,
                &[
                    ("http.requests_total", value.into()),
                    ("jobs.queued", value.into()),
                ],
            )
            .await
            .unwrap();
        }
        let window = HistoryQuery {
            since: Some(crate::v2::timestamp(now - 30)),
            step_seconds: Some("60".into()),
        }
        .validate(now)
        .unwrap();
        let history = read_pool(&pool, window, now).await.unwrap();
        assert_eq!(history.series.len(), 18);
        let counters = &history
            .series
            .iter()
            .find(|s| s.id == "http.requests_total")
            .unwrap()
            .points;
        assert_eq!(
            counters.iter().map(|p| p.v.as_str()).collect::<Vec<_>>(),
            ["80", "90", "0", "3"]
        );
        let gauges = &history
            .series
            .iter()
            .find(|s| s.id == "jobs.queued")
            .unwrap()
            .points;
        // Gauges have no reset to preserve: one point per bucket.
        assert_eq!(
            gauges.iter().map(|p| p.v.as_str()).collect::<Vec<_>>(),
            ["3"]
        );
        assert!(
            history
                .series
                .iter()
                .find(|s| s.id == "process.open_files")
                .unwrap()
                .points
                .is_empty()
        );
        assert!(
            history
                .series
                .iter()
                .find(|s| s.id == "storage.free_bytes")
                .unwrap()
                .points
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_samplers_own_writes_take_turns_on_the_fail_fast_pool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turns.sqlite3");
        // Kept open: a pool closing in the background checkpoints under an
        // exclusive lock (see `database`), which is not what this tests.
        let _setup = database(&path).await;
        let sampler = writer_pool(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        let writes = Writes::default();
        let tasks: Vec<_> = (0..24)
            .map(|i| {
                let (sampler, writes) = (sampler.clone(), writes.clone());
                tokio::spawn(async move {
                    let _turn = writes.0.lock().await;
                    record(
                        &sampler,
                        "run",
                        1_800_000_000_000 + i,
                        &[("http.requests_total", i.to_string())],
                    )
                    .await
                })
            })
            .collect();
        for task in tasks {
            task.await
                .unwrap()
                .expect("a sampler write never fails another");
        }
    }

    #[tokio::test]
    async fn persistence_retention_cap_and_fail_fast_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let pool = database(&path).await;
        let now = 1_800_000_000;
        let floor = (now - RETENTION_SECONDS) * 1_000;
        record(
            &pool,
            "old",
            floor - 1,
            &[("http.requests_total", "99".into())],
        )
        .await
        .unwrap();
        record(
            &pool,
            "old",
            floor,
            &[("http.requests_total", "100".into())],
        )
        .await
        .unwrap();
        prune(&pool, "new", now).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM operational_metrics")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        pool.close().await;
        let pool = reopen(&path).await;
        record(
            &pool,
            "new",
            now * 1_000,
            &[("http.requests_total", "0".into())],
        )
        .await
        .unwrap();
        let history = read_pool(&pool, HistoryQuery::default().validate(now).unwrap(), now)
            .await
            .unwrap();
        let points = &history
            .series
            .iter()
            .find(|s| s.id == "http.requests_total")
            .unwrap()
            .points;
        assert_eq!(
            points.iter().map(|p| p.v.as_str()).collect::<Vec<_>>(),
            ["100", "0"]
        );
        // The production sampler pool, while another connection holds the
        // writer: its sample fails at once instead of queueing.
        let sampler = writer_pool(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        let writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let blocked = tokio::time::timeout(
            Duration::from_millis(500),
            record(
                &sampler,
                "new",
                now * 1_000 + 1,
                &[("http.requests_total", "1".into())],
            ),
        )
        .await;
        assert!(
            blocked.unwrap().is_err(),
            "sampling should fail immediately under writer contention"
        );
        let read = tokio::time::timeout(
            Duration::from_millis(500),
            read_pool(
                &sampler,
                HistoryQuery::default().validate(now).unwrap(),
                now,
            ),
        )
        .await;
        assert!(
            read.unwrap().is_ok(),
            "WAL history reads should not acquire the writer"
        );
        writer.rollback().await.unwrap();
        // Even thousands of distinct runtime identities cannot defeat the cap.
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2100) INSERT INTO operational_metrics(series_id,sampled_at,run_id,value) SELECT 'http.requests_total',?+x,CAST(x AS TEXT),'0' FROM n")
            .bind(now*1_000-3_000).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let history = read_pool(&pool, HistoryQuery::default().validate(now).unwrap(), now)
            .await
            .unwrap();
        let points = &history
            .series
            .iter()
            .find(|s| s.id == "http.requests_total")
            .unwrap()
            .points;
        assert_eq!(points.len(), MAX_POINTS);
        assert_eq!(points.last().unwrap().v, "0");
        // Uncontended, the sampler pool writes.
        record(
            &sampler,
            "new",
            now * 1_000 + 2,
            &[("http.requests_total", "2".into())],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn failure_observation_dedup_and_counter_survive_lost_acknowledgment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("failures.sqlite3");
        let pool = database(&path).await;
        let now = 1_800_000_000;
        let observed = vec![
            ("scan".into(), "same-id".into()),
            ("processing".into(), "same-id".into()),
        ];
        record_observations(&pool, "old", now * 1_000, vec![], &observed)
            .await
            .unwrap();
        // Forget every caller-local result as if the timeout won after SQLite
        // durably committed. Reopening must not lose the count behind dedup.
        pool.close().await;
        let pool = reopen(&path).await;
        record_observations(&pool, "old", now * 1_000 + 10_000, vec![], &observed)
            .await
            .unwrap();
        record_observations(&pool, "old", now * 1_000 + 20_000, vec![], &[])
            .await
            .unwrap();
        record_observations(&pool, "new", now * 1_000 + 30_000, vec![], &[])
            .await
            .unwrap();
        let counts: Vec<String> = sqlx::query_scalar("SELECT value FROM operational_metrics WHERE series_id='jobs.failed_total' ORDER BY sampled_at,id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(counts, ["2", "2", "2", "0"]);
        prune(&pool, "new", now + 30).await.unwrap();
        let keys: i64 = sqlx::query_scalar("SELECT count(*) FROM operational_metric_failures")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(keys, 0, "previous run dedup keys are not retained history");
        let counters: Vec<(String, String)> =
            sqlx::query_as("SELECT run_id,failed_total FROM operational_metric_counters")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(counters, [("new".into(), "0".into())]);
    }

    #[tokio::test]
    async fn storage_points_stay_sparse_without_interpolation() {
        let dir = tempfile::tempdir().unwrap();
        let pool = database(&dir.path().join("sparse.sqlite3")).await;
        let now = 1_800_000_060;
        for offset in (0..=60).step_by(10) {
            record(
                &pool,
                "run",
                (now - 60 + offset) * 1_000,
                &[("process.resident_bytes", (offset + 100).to_string())],
            )
            .await
            .unwrap();
        }
        for offset in [0, 60] {
            record(
                &pool,
                "run",
                (now - 60 + offset) * 1_000,
                &[("storage.asset_bytes", (offset + 1_000).to_string())],
            )
            .await
            .unwrap();
        }
        let window = HistoryQuery {
            since: Some(crate::v2::timestamp(now - 60)),
            step_seconds: Some("10".into()),
        }
        .validate(now)
        .unwrap();
        let history = read_pool(&pool, window, now).await.unwrap();
        let process = &history
            .series
            .iter()
            .find(|s| s.id == "process.resident_bytes")
            .unwrap()
            .points;
        let storage = &history
            .series
            .iter()
            .find(|s| s.id == "storage.asset_bytes")
            .unwrap()
            .points;
        assert_eq!(process.len(), 7);
        assert_eq!(storage.len(), 2);
        assert_eq!(storage[0].t, crate::v2::timestamp(now - 60));
        assert_eq!(storage[1].t, crate::v2::timestamp(now));
        assert!(
            history
                .series
                .iter()
                .find(|s| s.id == "storage.free_bytes")
                .unwrap()
                .points
                .is_empty()
        );
    }

    #[test]
    fn storage_walk_observes_bytes_and_stops_on_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one"), [0; 7]).unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/two"), [0; 11]).unwrap();
        let cancel = CancellationToken::new();
        assert_eq!(
            bounded_cache_bytes(dir.path().to_path_buf(), &cancel).unwrap(),
            18
        );
        cancel.cancel();
        assert!(bounded_cache_bytes(dir.path().to_path_buf(), &cancel).is_err());
    }
}
