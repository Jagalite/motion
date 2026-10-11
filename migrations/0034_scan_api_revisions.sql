-- A02: revisions for the public scan demand representation.
ALTER TABLE scan_requests ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
CREATE TRIGGER scan_revision_demand_insert AFTER INSERT ON scan_demands BEGIN
 UPDATE scan_requests SET revision=revision+1 WHERE id=NEW.request_id; END;
CREATE TRIGGER scan_revision_demand_update AFTER UPDATE ON scan_demands BEGIN
 UPDATE scan_requests SET revision=revision+1 WHERE id=NEW.request_id; END;
CREATE TRIGGER scan_revision_job_update AFTER UPDATE ON jobs BEGIN
 UPDATE scan_requests SET revision=revision+1 WHERE id IN (
  SELECT request_id FROM scan_demands WHERE job_id=NEW.id OR (source_id=NEW.library_id AND status='pending'));
 END;
