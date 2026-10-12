-- Administrator playback monitoring. Observations only: nothing here
-- authorizes, admits, renews or ends a delivery; the delivery reducer in
-- crates/core decides those, and this records what it decided and what was
-- transferred.

-- One row per v2 delivery, written in the transaction that admits it,
-- updated with transfer counters while it is live, and finished with an end
-- reason when it leaves memory. A process that dies leaves rows unfinished;
-- startup recovery finishes them as interrupted with their last counters.
CREATE TABLE playback_monitor_sessions (
    delivery_id TEXT PRIMARY KEY,
    principal_id TEXT NOT NULL,
    profile_id TEXT NOT NULL,
    timeline_id TEXT NOT NULL,
    version_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('direct', 'remux', 'transcode')),
    client_network TEXT NOT NULL CHECK (client_network IN ('local', 'private', 'remote', 'unknown')),
    started_at INTEGER NOT NULL,
    last_activity_at INTEGER NOT NULL,
    ended_at INTEGER,
    end_reason TEXT CHECK (end_reason IN ('closed', 'lease_expired', 'error', 'server_stopped', 'interrupted')),
    bytes_sent INTEGER NOT NULL DEFAULT 0 CHECK (bytes_sent >= 0),
    peak_bps INTEGER NOT NULL DEFAULT 0 CHECK (peak_bps >= 0),
    watched_ms INTEGER NOT NULL DEFAULT 0 CHECK (watched_ms >= 0),
    start_position_ms INTEGER,
    end_position_ms INTEGER,
    stalls INTEGER NOT NULL DEFAULT 0 CHECK (stalls >= 0),
    -- A live row may already know why it is ending; a finished row always does.
    CHECK (ended_at IS NULL OR end_reason IS NOT NULL)
);
CREATE INDEX playback_monitor_history ON playback_monitor_sessions(ended_at, delivery_id) WHERE ended_at IS NOT NULL;
CREATE INDEX playback_monitor_device ON playback_monitor_sessions(principal_id, ended_at, delivery_id);
CREATE INDEX playback_monitor_profile ON playback_monitor_sessions(profile_id, ended_at, delivery_id);
CREATE INDEX playback_monitor_timeline ON playback_monitor_sessions(timeline_id, ended_at, delivery_id);

-- The monitor reads a live delivery's viewing status by delivery.
CREATE INDEX viewing_sessions_delivery ON viewing_sessions(delivery_id, updated_at);

-- Three playback series join the operational history. SQLite cannot alter a
-- CHECK constraint, so the table is rebuilt with every existing row.
CREATE TABLE operational_metrics_next (
    id INTEGER PRIMARY KEY,
    series_id TEXT NOT NULL CHECK (series_id IN (
        'process.cpu_seconds_total', 'process.resident_bytes', 'process.open_files',
        'http.requests_total', 'http.request_errors_total', 'http.requests_in_flight',
        'deliveries.active', 'jobs.queued', 'jobs.running', 'jobs.failed_total',
        'db.busy_total', 'storage.database_bytes', 'storage.asset_bytes',
        'storage.temporary_bytes', 'storage.free_bytes',
        'playback.sessions_active', 'playback.bytes_sent_total', 'playback.transcodes_active'
    )),
    sampled_at INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    value TEXT NOT NULL CHECK (length(value) BETWEEN 1 AND 128),
    UNIQUE (series_id, run_id, sampled_at)
);
INSERT INTO operational_metrics_next(id, series_id, sampled_at, run_id, value)
    SELECT id, series_id, sampled_at, run_id, value FROM operational_metrics;
DROP TABLE operational_metrics;
ALTER TABLE operational_metrics_next RENAME TO operational_metrics;
CREATE INDEX operational_metrics_history ON operational_metrics(series_id, sampled_at, id);
CREATE INDEX operational_metrics_retention ON operational_metrics(sampled_at, id);
