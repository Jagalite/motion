-- Immutable, explicit Motion physical-source -> native source binding. A native
-- bind committed before this journal row can be retried only with exact identity.
CREATE TABLE catabolic_source_mappings (
 library_id TEXT PRIMARY KEY REFERENCES libraries(id),
 profile TEXT NOT NULL,
 native_source TEXT NOT NULL,
 mapping_json TEXT NOT NULL,
 created_at INTEGER NOT NULL,
 UNIQUE(profile,native_source)
);
CREATE TRIGGER catabolic_source_mapping_immutable_update BEFORE UPDATE ON catabolic_source_mappings
 BEGIN SELECT RAISE(ABORT,'catabolic source mapping is immutable'); END;
CREATE TRIGGER catabolic_source_mapping_immutable_delete BEFORE DELETE ON catabolic_source_mappings
 BEGIN SELECT RAISE(ABORT,'catabolic source mapping is immutable'); END;
