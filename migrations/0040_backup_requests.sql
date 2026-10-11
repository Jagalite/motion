CREATE TABLE backup_jobs (
 id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL DEFAULT 1,
 phase TEXT NOT NULL DEFAULT 'queued' CHECK(phase IN ('queued','running','cancelling','cancelled','completed','failed')),
 attempt INTEGER NOT NULL DEFAULT 0,
 requester_id TEXT NOT NULL,
 credential_hash TEXT,
 ingress_id TEXT,
 include_generated_media INTEGER NOT NULL CHECK(include_generated_media IN (0,1)),
 created_at INTEGER NOT NULL,
 manifest_digest TEXT,
 error TEXT
);
CREATE TRIGGER backup_job_revision AFTER UPDATE OF phase,attempt,error,manifest_digest ON backup_jobs BEGIN
 UPDATE backup_jobs SET revision=revision+1 WHERE id=NEW.id;
 INSERT INTO change_events(topic,resource_id) VALUES ('job',NEW.id); END;
DROP VIEW api_jobs;
CREATE VIEW api_jobs AS
 SELECT id,revision,'scan' AS kind,phase,attempt,NULL AS progress,error,NULL AS result_id,requester_id FROM jobs
 UNION ALL SELECT id,revision,'rendition',phase,attempt,NULL,error,output_file_id,requester_id FROM processing_jobs
 UNION ALL SELECT id,revision,'metadata',phase,attempt,NULL,error,CASE WHEN phase='completed' THEN item_id END,requester_id FROM metadata_jobs
 UNION ALL SELECT id,revision,'backup',phase,attempt,NULL,error,CASE WHEN phase='completed' THEN id END,requester_id FROM backup_jobs;
