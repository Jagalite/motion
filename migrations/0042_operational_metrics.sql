-- Operational observations never authorize or drive application transitions.
-- Millisecond timestamps retain observations across rapid process restarts.
CREATE TABLE operational_metrics (
    id INTEGER PRIMARY KEY,
    series_id TEXT NOT NULL CHECK (series_id IN (
        'process.cpu_seconds_total', 'process.resident_bytes', 'process.open_files',
        'http.requests_total', 'http.request_errors_total', 'http.requests_in_flight',
        'deliveries.active', 'jobs.queued', 'jobs.running', 'jobs.failed_total',
        'db.busy_total', 'storage.database_bytes', 'storage.asset_bytes',
        'storage.temporary_bytes', 'storage.free_bytes'
    )),
    sampled_at INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    value TEXT NOT NULL CHECK (length(value) BETWEEN 1 AND 128),
    UNIQUE (series_id, run_id, sampled_at)
);
CREATE INDEX operational_metrics_history ON operational_metrics(series_id, sampled_at, id);
CREATE INDEX operational_metrics_retention ON operational_metrics(sampled_at, id);

-- Keep deduplication out of an ever-growing process heap. Previous runs are
-- removed in bounded batches; the sample values, not these keys, are history.
CREATE TABLE operational_metric_failures (
    run_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('scan', 'processing')),
    job_id TEXT NOT NULL,
    PRIMARY KEY (run_id, source, job_id)
);

-- Counter and deduplication commit together, including when a timed-out caller
-- never receives SQLite's COMMIT acknowledgment.
CREATE TABLE operational_metric_counters (
    run_id TEXT PRIMARY KEY,
    failed_total TEXT NOT NULL
) WITHOUT ROWID;
