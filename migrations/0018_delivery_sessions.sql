-- CHANGE REQUEST from A07 for A02: the number 0015 is provisional (0010-0014
-- are taken on other branches); A02 owns migration numbering and may renumber.
-- Delivery sessions are retained as diagnostic/recovery intent (plan 10.1):
-- after a restart they read as interrupted; live transports never resume.
CREATE TABLE delivery_sessions (
 id TEXT PRIMARY KEY,
 file_id TEXT NOT NULL,
 file_revision TEXT NOT NULL,
 -- Core delivery revision; snapshots never replace a newer one.
 revision INTEGER NOT NULL,
 status TEXT NOT NULL,
 -- Serialized playscale_core::delivery::Delivery.
 state_json TEXT NOT NULL,
 updated_at INTEGER NOT NULL
);
CREATE INDEX delivery_sessions_status ON delivery_sessions(status, updated_at);
