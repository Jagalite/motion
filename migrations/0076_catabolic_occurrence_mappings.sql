CREATE TABLE catabolic_occurrence_mappings (
 import_id TEXT NOT NULL,
 motion_file_id TEXT NOT NULL,
 native_profile TEXT NOT NULL,
 native_file_id TEXT NOT NULL,
 mapping_json TEXT NOT NULL,
 PRIMARY KEY(import_id,motion_file_id),
 UNIQUE(import_id,native_profile,native_file_id)
);
CREATE TRIGGER catabolic_occurrence_mapping_immutable_update BEFORE UPDATE ON catabolic_occurrence_mappings BEGIN
 SELECT RAISE(ABORT,'catabolic occurrence mappings are immutable'); END;
CREATE TRIGGER catabolic_occurrence_mapping_immutable_delete BEFORE DELETE ON catabolic_occurrence_mappings BEGIN
 SELECT RAISE(ABORT,'catabolic occurrence mappings are immutable'); END;
