-- Completion 5D: what a download's own device reports about its local copy
-- (playscale_core::downloads::accept_transfer). Device-owned facts, stored
-- as reported in the device's order; the server never derives them, and a
-- ready download is not a verified local copy. One row per download: only
-- the download's device reports.
CREATE TABLE download_transfers (
 download_id TEXT PRIMARY KEY,
 sequence INTEGER NOT NULL CHECK(sequence >= 1),
 download_revision INTEGER NOT NULL CHECK(download_revision >= 1),
 state TEXT NOT NULL CHECK(state IN ('queued','transferring','verified_local','failed','removed')),
 bytes_received INTEGER NOT NULL CHECK(bytes_received >= 0),
 reported_at INTEGER NOT NULL
);
