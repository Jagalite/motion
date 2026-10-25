-- Native scan demand custody is application control state, not legacy catalog
-- ownership. Exact accepted source/principal proof remains immutable on replay.
CREATE TABLE catabolic_scan_requests (
 admission INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 library_id TEXT NOT NULL,
 requester_id TEXT NOT NULL,
 scope_json TEXT NOT NULL,
 verify INTEGER NOT NULL CHECK(verify IN (0,1)),
 require_complete INTEGER NOT NULL CHECK(require_complete IN (0,1)),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
 created_at INTEGER NOT NULL
);
CREATE TABLE catabolic_scan_demands (
 id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL REFERENCES catabolic_scan_requests(id),
 source_id TEXT NOT NULL,
 mapping_json TEXT NOT NULL,
 barrier INTEGER NOT NULL CHECK(barrier>0),
 phase TEXT NOT NULL CHECK(phase IN ('queued','running','cancelling','completed','failed','cancelled')),
 status TEXT NOT NULL CHECK(status IN ('pending','complete','partial','failed','cancelled')),
 attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt>=0 AND attempt<=32),
 started_barrier INTEGER,
 result_json TEXT,
 error TEXT,
 UNIQUE(request_id,source_id)
);
CREATE INDEX catabolic_scan_pending ON catabolic_scan_demands(status,phase,source_id);
CREATE UNIQUE INDEX catabolic_scan_one_running ON catabolic_scan_demands(source_id) WHERE phase IN ('running','cancelling');
CREATE TRIGGER catabolic_scan_request_identity BEFORE UPDATE ON catabolic_scan_requests
 WHEN NEW.admission IS NOT OLD.admission OR NEW.id IS NOT OLD.id
 OR NEW.library_id IS NOT OLD.library_id OR NEW.requester_id IS NOT OLD.requester_id
 OR NEW.scope_json IS NOT OLD.scope_json OR NEW.verify IS NOT OLD.verify
 OR NEW.require_complete IS NOT OLD.require_complete OR NEW.created_at IS NOT OLD.created_at
 BEGIN SELECT RAISE(ABORT,'native scan request identity is immutable'); END;
CREATE TRIGGER catabolic_scan_demand_identity BEFORE UPDATE ON catabolic_scan_demands
 WHEN NEW.id IS NOT OLD.id OR NEW.request_id IS NOT OLD.request_id
 OR NEW.source_id IS NOT OLD.source_id OR NEW.mapping_json IS NOT OLD.mapping_json
 OR NEW.barrier IS NOT OLD.barrier
 BEGIN SELECT RAISE(ABORT,'native scan demand identity is immutable'); END;
CREATE TRIGGER catabolic_scan_request_retained BEFORE DELETE ON catabolic_scan_requests
 BEGIN SELECT RAISE(ABORT,'native scan requests are retained'); END;
CREATE TRIGGER catabolic_scan_demand_retained BEFORE DELETE ON catabolic_scan_demands
 BEGIN SELECT RAISE(ABORT,'native scan demands are retained'); END;
CREATE TRIGGER catabolic_scan_revision AFTER UPDATE ON catabolic_scan_demands
 BEGIN UPDATE catabolic_scan_requests SET revision=revision+1 WHERE id=NEW.request_id; END;
