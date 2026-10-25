-- Immutable bounded export partitions share one frozen source and audit.
CREATE TABLE catabolic_export_plans (
    root_import_id TEXT PRIMARY KEY,
    source_snapshot_id TEXT NOT NULL,
    source_namespace TEXT NOT NULL,
    audit_digest TEXT NOT NULL,
    plan_json TEXT NOT NULL CHECK(json_valid(plan_json))
);
CREATE TABLE catabolic_export_plan_parts (
    import_id TEXT PRIMARY KEY REFERENCES catabolic_motion_exports(import_id),
    root_import_id TEXT NOT NULL REFERENCES catabolic_export_plans(root_import_id),
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    snapshot_digest TEXT NOT NULL,
    UNIQUE(root_import_id, ordinal)
);
CREATE TRIGGER catabolic_export_plans_no_update BEFORE UPDATE ON catabolic_export_plans BEGIN SELECT RAISE(ABORT, 'immutable export plan'); END;
CREATE TRIGGER catabolic_export_plans_no_delete BEFORE DELETE ON catabolic_export_plans BEGIN SELECT RAISE(ABORT, 'immutable export plan'); END;
CREATE TRIGGER catabolic_export_parts_no_update BEFORE UPDATE ON catabolic_export_plan_parts BEGIN SELECT RAISE(ABORT, 'immutable export partition'); END;
CREATE TRIGGER catabolic_export_parts_no_delete BEFORE DELETE ON catabolic_export_plan_parts BEGIN SELECT RAISE(ABORT, 'immutable export partition'); END;
