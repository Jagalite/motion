CREATE TABLE api_schedules (
 id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL DEFAULT 1,
 library_id TEXT NOT NULL REFERENCES catalog_libraries(id),
 task TEXT NOT NULL CHECK(task IN ('scan','metadata_refresh','preview','optimize')),
 interval_seconds INTEGER NOT NULL CHECK(interval_seconds BETWEEN 60 AND 31536000),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 next_run INTEGER NOT NULL,
 requester_id TEXT NOT NULL,
 credential_hash TEXT,
 ingress_id TEXT
);
CREATE TRIGGER api_schedule_insert AFTER INSERT ON api_schedules BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('schedules',NEW.id); END;
CREATE TRIGGER api_schedule_update AFTER UPDATE OF revision ON api_schedules BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('schedules',NEW.id); END;
CREATE TRIGGER api_schedule_delete AFTER DELETE ON api_schedules BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('schedules',OLD.id); END;
ALTER TABLE preparations ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
CREATE TRIGGER preparation_job_revision AFTER UPDATE ON processing_jobs BEGIN
 UPDATE preparations SET revision=revision+1 WHERE job_id=NEW.id; END;
CREATE TRIGGER preparation_catalog_revision AFTER INSERT ON change_events
WHEN NEW.topic IN ('catalog','libraries','sources') BEGIN
 UPDATE preparations SET revision=revision+1; END;
CREATE TRIGGER preparation_binding_insert AFTER INSERT ON version_files BEGIN
 UPDATE preparations SET revision=revision+1; END;
CREATE TRIGGER preparation_binding_update AFTER UPDATE ON version_files BEGIN
 UPDATE preparations SET revision=revision+1; END;
CREATE TRIGGER preparation_binding_delete AFTER DELETE ON version_files BEGIN
 UPDATE preparations SET revision=revision+1; END;
