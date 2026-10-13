-- Completion 6C: at most one active preview job per file revision (never
-- duplicated work). Any duplicates are cancelled first, keeping the oldest.
UPDATE preview_jobs SET phase='cancelled'
 WHERE phase IN ('queued','running','cancelling')
 AND rowid NOT IN (SELECT min(rowid) FROM preview_jobs WHERE phase IN ('queued','running','cancelling') GROUP BY file_id,file_revision);
DROP INDEX preview_jobs_active;
CREATE UNIQUE INDEX preview_jobs_active ON preview_jobs(file_id,file_revision) WHERE phase IN ('queued','running','cancelling');
