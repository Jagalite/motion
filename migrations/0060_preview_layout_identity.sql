-- Bind jobs and publications to the technical facts used for their layout.
-- Older jobs must be requested again; they have no trustworthy snapshot.
ALTER TABLE preview_jobs ADD COLUMN layout_identity TEXT NOT NULL DEFAULT '';
ALTER TABLE previews ADD COLUMN layout_identity TEXT NOT NULL DEFAULT '';
UPDATE preview_jobs SET phase='failed',error='preview_layout_changed'
 WHERE phase IN ('queued','running','cancelling');
CREATE TRIGGER preview_job_insert AFTER INSERT ON preview_jobs BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('job',NEW.id); END;
CREATE TABLE preview_retention_cursor (id INTEGER PRIMARY KEY CHECK(id=1), after_file TEXT NOT NULL);
INSERT INTO preview_retention_cursor VALUES (1,'');
