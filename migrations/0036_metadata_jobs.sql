-- A02: durable local NFO refresh jobs with current authorization witnesses.
CREATE TABLE metadata_jobs (
 id TEXT PRIMARY KEY,
 item_id TEXT NOT NULL REFERENCES items(id),
 requester_id TEXT NOT NULL,
 credential_hash TEXT,
 ingress_id TEXT,
 revision INTEGER NOT NULL DEFAULT 1,
 phase TEXT NOT NULL CHECK(phase IN ('queued','running','cancelling','cancelled','completed','failed')),
 attempt INTEGER NOT NULL DEFAULT 0,
 error TEXT,
 created_at INTEGER NOT NULL
);
CREATE TRIGGER metadata_job_revision AFTER UPDATE OF phase,attempt,error ON metadata_jobs BEGIN
 UPDATE metadata_jobs SET revision=revision+1 WHERE id=NEW.id;
 INSERT INTO change_events(topic,resource_id) VALUES ('job',NEW.id); END;
DROP VIEW api_jobs;
CREATE VIEW api_jobs AS
 SELECT id,revision,'scan' AS kind,phase,attempt,NULL AS progress,error,NULL AS result_id,requester_id FROM jobs
 UNION ALL
 SELECT id,revision,'rendition',phase,attempt,NULL,error,output_file_id,requester_id FROM processing_jobs
 UNION ALL
 SELECT id,revision,'metadata',phase,attempt,NULL,error,CASE WHEN phase='completed' THEN item_id END,requester_id FROM metadata_jobs;
