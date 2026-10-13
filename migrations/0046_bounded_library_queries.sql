-- Bounded library queries (completion item 2).
--
-- Scan history is paged newest first by a durable admission sequence. SQLite
-- rowids are not enough: deleting a library's newest requests lets the next
-- insert reuse their rowids. The counter only grows; every insert (any
-- writer) takes the next value through the trigger. Existing rows keep their
-- insertion order.
ALTER TABLE scan_requests ADD COLUMN admission INTEGER;
UPDATE scan_requests SET admission=rowid;
CREATE TABLE scan_admissions (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 last INTEGER NOT NULL CHECK(last >= 0)
);
INSERT INTO scan_admissions(singleton,last) VALUES (1,coalesce((SELECT max(admission) FROM scan_requests),0));
CREATE TRIGGER scan_requests_admission AFTER INSERT ON scan_requests WHEN NEW.admission IS NULL BEGIN
 UPDATE scan_admissions SET last=last+1 WHERE singleton=1;
 UPDATE scan_requests SET admission=(SELECT last FROM scan_admissions WHERE singleton=1) WHERE rowid=NEW.rowid;
END;
CREATE UNIQUE INDEX scan_requests_admission ON scan_requests(admission);
CREATE INDEX scan_requests_library_admission ON scan_requests(library_id,admission);
-- Renditions by timeline. Settled scan history is pruned by
-- playscale_core::demands::prunable_requests at admission.
CREATE INDEX preparations_timeline ON preparations(timeline_id,job_id);
