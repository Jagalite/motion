-- A13: device-owned offline downloads and offline viewing reconciliation
-- (playscale_core::downloads). 0042 is proposed by PR #29; this lane takes
-- 0043 so neither renumbers. Expiry is derived from current revisions and
-- never stored. Transfer progress is device-owned and not recorded here.

CREATE TABLE downloads (
 id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL CHECK(revision >= 1),
 principal_id TEXT NOT NULL,
 device_id TEXT NOT NULL,
 -- No foreign keys: profile deletion removes these rows explicitly, and a
 -- catalog edit must not be blocked by a device's pinned copy.
 profile_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL,
 timeline_revision INTEGER NOT NULL CHECK(timeline_revision >= 1),
 version_id TEXT NOT NULL,
 file_id TEXT NOT NULL,
 file_revision TEXT NOT NULL,
 content_type TEXT NOT NULL,
 size_bytes INTEGER NOT NULL CHECK(size_bytes > 0),
 duration_ms INTEGER NOT NULL CHECK(duration_ms > 0),
 state TEXT NOT NULL CHECK(state IN ('ready','cancelled')),
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL
);
CREATE INDEX downloads_device ON downloads(device_id,principal_id,id);
CREATE INDEX downloads_profile ON downloads(profile_id);

-- Byte grants issued through a download are revoked when it is cancelled.
ALTER TABLE content_tickets ADD COLUMN download_id TEXT;
CREATE INDEX content_tickets_download ON content_tickets(download_id) WHERE download_id IS NOT NULL;

-- The next unconsumed device sequence per device and profile. One device
-- cache root holds one profile, so its consecutive log maps to one row.
CREATE TABLE offline_sequences (
 device_id TEXT NOT NULL,
 profile_id TEXT NOT NULL,
 next_sequence INTEGER NOT NULL CHECK(next_sequence >= 1),
 PRIMARY KEY(device_id,profile_id)
);

-- Every consumed offline event and its outcome. Exact retries replay the
-- stored outcome; a reused sequence or identity with other content conflicts.
-- history_only and rejected events are retained here as offline history.
CREATE TABLE offline_events (
 device_id TEXT NOT NULL,
 profile_id TEXT NOT NULL,
 sequence INTEGER NOT NULL CHECK(sequence >= 1),
 event_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL,
 event_json TEXT NOT NULL,
 outcome TEXT NOT NULL CHECK(outcome IN ('applied','history_only','rejected')),
 code TEXT,
 created_at INTEGER NOT NULL,
 PRIMARY KEY(device_id,profile_id,sequence),
 UNIQUE(device_id,profile_id,event_id)
);

-- The viewing revision this device's applied offline chain last produced on
-- a timeline, and the base it started from.
CREATE TABLE offline_lineage (
 device_id TEXT NOT NULL,
 profile_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL,
 base_viewing_revision INTEGER NOT NULL CHECK(base_viewing_revision >= 0),
 base_manual_epoch INTEGER NOT NULL CHECK(base_manual_epoch >= 0),
 produced_revision INTEGER NOT NULL CHECK(produced_revision >= 1),
 PRIMARY KEY(device_id,profile_id,timeline_id)
);
