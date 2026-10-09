-- Per-attempt directory coverage. A published attempt is complete or partial;
-- absence was inferred only inside directories it listed completely.
ALTER TABLE jobs ADD COLUMN outcome TEXT CHECK(outcome IN ('complete','partial'));
ALTER TABLE jobs ADD COLUMN complete_directories INTEGER NOT NULL DEFAULT 0;
ALTER TABLE jobs ADD COLUMN incomplete_directories INTEGER NOT NULL DEFAULT 0;
-- Bounded diagnostic sample of unproven directories (source-relative, no root path).
CREATE TABLE scan_incomplete_directories (
 job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
 directory TEXT NOT NULL,
 reason TEXT NOT NULL CHECK(reason IN ('unreadable','unreadable_entry','mount_boundary','non_utf8_path','file_limit','inspection_failed')),
 PRIMARY KEY(job_id,directory)
);
