-- Shared scan demands. A request (the public Scan) groups one demand per
-- source; demands are satisfied by physical attempts (jobs) that started at or
-- after the demand's freshness barrier with a strong-enough mode.
ALTER TABLE libraries ADD COLUMN scan_barrier INTEGER NOT NULL DEFAULT 0;
ALTER TABLE jobs ADD COLUMN started_barrier INTEGER;

-- Per source: at most one running attempt and one queued follow-up.
DROP INDEX one_active_scan;
CREATE UNIQUE INDEX one_running_scan ON jobs(library_id) WHERE phase IN ('running','cancelling');
CREATE UNIQUE INDEX one_queued_scan ON jobs(library_id) WHERE phase='queued';

CREATE TABLE scan_requests (
 id TEXT PRIMARY KEY,
 library_id TEXT NOT NULL REFERENCES catalog_libraries(id),
 require_complete INTEGER NOT NULL CHECK(require_complete IN (0,1)),
 created_at INTEGER NOT NULL
);
CREATE TABLE scan_demands (
 id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL REFERENCES scan_requests(id) ON DELETE CASCADE,
 source_id TEXT NOT NULL REFERENCES libraries(id),
 barrier INTEGER NOT NULL CHECK(barrier >= 0),
 verify INTEGER NOT NULL CHECK(verify IN (0,1)),
 status TEXT NOT NULL CHECK(status IN ('pending','complete','partial','failed','cancelled')),
 -- The attempt that answered the demand (null while pending or if cancelled).
 job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL,
 error TEXT,
 UNIQUE(request_id,source_id)
);
CREATE INDEX scan_demands_pending ON scan_demands(source_id,status);
CREATE TRIGGER event_scan_requests_insert AFTER INSERT ON scan_requests BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('scan',NEW.id); END;
CREATE TRIGGER event_scan_demands_update AFTER UPDATE ON scan_demands BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('scan',NEW.request_id); END;
