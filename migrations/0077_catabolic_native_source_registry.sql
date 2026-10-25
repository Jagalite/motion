-- Native sources are independent of the frozen legacy source projection.
ALTER TABLE catabolic_source_mappings RENAME TO catabolic_source_mappings_v70;
DROP TRIGGER catabolic_source_mapping_immutable_update;
DROP TRIGGER catabolic_source_mapping_immutable_delete;
CREATE TABLE catabolic_source_mappings (
 library_id TEXT PRIMARY KEY,
 profile TEXT NOT NULL,
 native_source TEXT NOT NULL,
 mapping_json TEXT NOT NULL,
 created_at INTEGER NOT NULL,
 UNIQUE(profile,native_source)
);
INSERT INTO catabolic_source_mappings SELECT * FROM catabolic_source_mappings_v70;
DROP TABLE catabolic_source_mappings_v70;
CREATE TRIGGER catabolic_source_mapping_immutable_update BEFORE UPDATE ON catabolic_source_mappings BEGIN SELECT RAISE(ABORT,'catabolic source mapping is immutable'); END;
CREATE TRIGGER catabolic_source_mapping_immutable_delete BEFORE DELETE ON catabolic_source_mappings BEGIN SELECT RAISE(ABORT,'catabolic source mapping is immutable'); END;
CREATE TABLE catabolic_source_registry (
 id TEXT PRIMARY KEY,
 descriptor_json TEXT NOT NULL,
 created_at INTEGER NOT NULL
);
CREATE TRIGGER catabolic_source_registry_immutable_update BEFORE UPDATE ON catabolic_source_registry BEGIN SELECT RAISE(ABORT,'native source registry is immutable'); END;
CREATE TRIGGER catabolic_source_registry_immutable_delete BEFORE DELETE ON catabolic_source_registry BEGIN SELECT RAISE(ABORT,'native source registry is immutable'); END;
