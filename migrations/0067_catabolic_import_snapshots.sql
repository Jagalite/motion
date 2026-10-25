-- Complete migration inputs are durable before any native staging command.
CREATE TABLE catabolic_import_snapshots (
 import_id TEXT PRIMARY KEY,
 input_digest TEXT NOT NULL,
 snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json)),
 created_at INTEGER NOT NULL
);
CREATE TRIGGER catabolic_import_snapshots_no_update BEFORE UPDATE ON catabolic_import_snapshots BEGIN
 SELECT RAISE(ABORT, 'Catabolic import snapshots are immutable'); END;
CREATE TRIGGER catabolic_import_snapshots_no_delete BEFORE DELETE ON catabolic_import_snapshots BEGIN
 SELECT RAISE(ABORT, 'Catabolic import identities cannot be recycled'); END;
