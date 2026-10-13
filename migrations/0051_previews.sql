-- Completion 6C: trick-play previews. A preview job renders thumbnail
-- sheets of one exact file revision (core `previews`); a published preview
-- is keyed by that revision and removed once it is no longer current.
CREATE TABLE preview_jobs (
 id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL DEFAULT 1,
 phase TEXT NOT NULL DEFAULT 'queued' CHECK(phase IN ('queued','running','cancelling','cancelled','completed','failed')),
 attempt INTEGER NOT NULL DEFAULT 0,
 file_id TEXT NOT NULL,
 file_revision TEXT NOT NULL,
 requester_id TEXT NOT NULL,
 credential_hash TEXT,
 ingress_id TEXT,
 created_at INTEGER NOT NULL,
 error TEXT
);
-- At most one active job per revision: never duplicated work.
CREATE UNIQUE INDEX preview_jobs_active ON preview_jobs(file_id,file_revision) WHERE phase IN ('queued','running','cancelling');
CREATE TRIGGER preview_job_revision AFTER UPDATE OF phase,attempt,error ON preview_jobs BEGIN
 UPDATE preview_jobs SET revision=revision+1 WHERE id=NEW.id;
 INSERT INTO change_events(topic,resource_id) VALUES ('job',NEW.id); END;
CREATE TABLE previews (
 file_id TEXT NOT NULL,
 file_revision TEXT NOT NULL,
 job_id TEXT NOT NULL,
 interval_ms INTEGER NOT NULL CHECK(interval_ms > 0),
 tile_width INTEGER NOT NULL,
 tile_height INTEGER NOT NULL,
 columns INTEGER NOT NULL,
 rows INTEGER NOT NULL,
 tiles INTEGER NOT NULL,
 sheets INTEGER NOT NULL,
 created_at INTEGER NOT NULL,
 PRIMARY KEY(file_id,file_revision)
);
DROP VIEW api_jobs;
CREATE VIEW api_jobs AS
 SELECT id,revision,'scan' AS kind,phase,attempt,NULL AS progress,error,NULL AS result_id,requester_id FROM jobs
 UNION ALL SELECT id,revision,'rendition',phase,attempt,NULL,error,output_file_id,requester_id FROM processing_jobs
 UNION ALL SELECT id,revision,'metadata',phase,attempt,NULL,error,CASE WHEN phase='completed' THEN item_id END,requester_id FROM metadata_jobs
 UNION ALL SELECT id,revision,'backup',phase,attempt,NULL,error,CASE WHEN phase='completed' THEN id END,requester_id FROM backup_jobs
 UNION ALL SELECT id,revision,'preview',phase,attempt,NULL,error,CASE WHEN phase='completed' THEN file_id END,requester_id FROM preview_jobs;
