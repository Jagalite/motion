-- A07: the v2 principal and context each planned delivery was admitted for
-- (playscale_core::playback_session::Owner). Number 0033 allocated by the
-- A02/PR #4 owner. Written in the admission transaction with the delivery's
-- first record, so a delivery read after a restart reports its interrupted
-- state to its owner instead of disappearing. v1 deliveries have no owner row
-- and stay invisible to v2 callers.
CREATE TABLE delivery_owners (
 delivery_id TEXT PRIMARY KEY REFERENCES delivery_sessions(id) ON DELETE CASCADE,
 principal_id TEXT NOT NULL,
 profile_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL,
 version_id TEXT NOT NULL,
 file_id TEXT NOT NULL,
 file_revision TEXT NOT NULL,
 created_at INTEGER NOT NULL
);
CREATE INDEX delivery_owners_principal ON delivery_owners(principal_id,created_at);
