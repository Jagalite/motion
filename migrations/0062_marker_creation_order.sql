-- Legacy markers retain their deterministic earliest-start/ID tie break (0).
-- New writes allocate a timeline-local increasing order under BEGIN IMMEDIATE.
-- Manual intro/recap/credits corrections then supersede earlier manual values
-- without deleting their provenance or relying on SQLite rowid stability.
ALTER TABLE markers ADD COLUMN creation_order INTEGER NOT NULL DEFAULT 0 CHECK(creation_order>=0);
