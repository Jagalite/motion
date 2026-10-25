-- Only the consistent Motion source exporter writes this provenance marker.
-- Imported JSON alone cannot authorize reconstruction of typed owner grants.
CREATE TABLE catabolic_motion_exports (
 import_id TEXT PRIMARY KEY REFERENCES catabolic_import_snapshots(import_id),
 input_digest TEXT NOT NULL,
 format_version INTEGER NOT NULL CHECK(format_version=1)
);
CREATE TRIGGER catabolic_motion_exports_no_update BEFORE UPDATE ON catabolic_motion_exports BEGIN
 SELECT RAISE(ABORT, 'Motion export provenance is immutable'); END;
CREATE TRIGGER catabolic_motion_exports_no_delete BEFORE DELETE ON catabolic_motion_exports BEGIN
 SELECT RAISE(ABORT, 'Motion export provenance cannot be recycled'); END;
