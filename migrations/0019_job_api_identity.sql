-- Older admissions did not persist a requesting principal. Keep that absence
-- explicit: legacy jobs are visible/manageable only to administrators, never
-- retroactively attributed to whichever device happens to query them.
ALTER TABLE jobs ADD COLUMN requester_id TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE jobs ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>=1);
ALTER TABLE processing_jobs ADD COLUMN requester_id TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE processing_jobs ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>=1);
CREATE TRIGGER job_api_revision AFTER UPDATE OF phase,attempt,error,outcome,reused_files,inspected_files,complete_directories,incomplete_directories ON jobs BEGIN
 UPDATE jobs SET revision=revision+1 WHERE id=NEW.id;
END;
CREATE TRIGGER processing_api_revision AFTER UPDATE OF phase,attempt,error,progress_seconds,output_file_id,expired,cache_cleaned ON processing_jobs BEGIN
 UPDATE processing_jobs SET revision=revision+1 WHERE id=NEW.id;
END;
CREATE VIEW api_jobs AS
 SELECT id,revision,'scan' AS kind,phase,attempt,NULL AS progress,error,NULL AS result_id,requester_id FROM jobs
 UNION ALL
 SELECT id,revision,'rendition',phase,attempt,NULL,error,output_file_id,requester_id FROM processing_jobs;
