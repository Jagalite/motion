-- Completion 5C: each timeline's viewing as it was restored, captured when a
-- backup is restored (db::restore). Offline progress recorded before the
-- restore and kept by the person is pinned to this baseline, so any viewing
-- written on the restored server since then is newer and wins
-- (downloads::reconcile answers viewing_newer). A timeline without a row had
-- no viewing when restored. The baseline describes only the history named in
-- restore_baseline_epoch; for any other history it is unknown.
CREATE TABLE restore_baselines (
 profile_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision >= 0),
 manual_epoch INTEGER NOT NULL CHECK(manual_epoch >= 0),
 PRIMARY KEY(profile_id,timeline_id)
);
CREATE TABLE restore_baseline_epoch (
 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
 restore_epoch TEXT NOT NULL
);
