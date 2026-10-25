-- Native receipt-time heads anchor monotonic Motion CAS revisions. A later
-- current native head is not evidence of the activation baseline.
CREATE TABLE catabolic_revision_bridges (
 native_id TEXT PRIMARY KEY,
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 motion_base INTEGER NOT NULL CHECK(motion_base>=1),
 native_base INTEGER NOT NULL CHECK(native_base>=1),
 proof_json TEXT NOT NULL CHECK(json_valid(proof_json))
);
CREATE TRIGGER catabolic_revision_bridge_no_update BEFORE UPDATE ON catabolic_revision_bridges BEGIN SELECT RAISE(ABORT,'native revision bridge is immutable'); END;
CREATE TRIGGER catabolic_revision_bridge_no_delete BEFORE DELETE ON catabolic_revision_bridges BEGIN SELECT RAISE(ABORT,'native revision bridge is retained'); END;
