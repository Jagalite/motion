-- Trusted source-reference proof is captured atomically with the immutable export.
-- It is host evidence, never inferred from caller-provided import JSON.
CREATE TABLE catabolic_reference_audits (
 import_id TEXT PRIMARY KEY REFERENCES catabolic_motion_exports(import_id),
 input_digest TEXT NOT NULL,
 audit_json TEXT NOT NULL CHECK(json_valid(audit_json))
);
CREATE TRIGGER catabolic_reference_audits_immutable_update BEFORE UPDATE ON catabolic_reference_audits
BEGIN SELECT RAISE(ABORT,'Catabolic reference audit is immutable'); END;
CREATE TRIGGER catabolic_reference_audits_immutable_delete BEFORE DELETE ON catabolic_reference_audits
BEGIN SELECT RAISE(ABORT,'Catabolic reference audit is immutable'); END;
