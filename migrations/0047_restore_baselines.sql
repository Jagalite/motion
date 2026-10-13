-- Completion 5C: each timeline's viewing as it was restored. A restore (the
-- supported scripts/backup.py restore, or db::restore_into) rotates the
-- restore epoch and records it here as pending, in one transaction; the
-- first server start after it captures the baseline from timeline_viewing
-- after migrations and before anything is served (db::connect), so it is
-- exactly the restored state. Offline progress recorded before the restore
-- and kept by the person is pinned to this baseline, so any viewing written
-- on the restored server since then is newer and wins (viewing_newer). A
-- timeline without a row had no viewing when restored. The baseline counts
-- only when captured for the current restore epoch. IF NOT EXISTS: the
-- restore tool creates these tables in an older backup with this same DDL.
CREATE TABLE IF NOT EXISTS restore_baselines (
 profile_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision >= 0),
 manual_epoch INTEGER NOT NULL CHECK(manual_epoch >= 0),
 PRIMARY KEY(profile_id,timeline_id)
);
CREATE TABLE IF NOT EXISTS restore_baseline_epoch (
 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
 restore_epoch TEXT NOT NULL,
 captured INTEGER NOT NULL DEFAULT 0 CHECK(captured IN (0,1))
);
