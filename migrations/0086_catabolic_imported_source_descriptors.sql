-- Immutable physical source descriptors acknowledged only from activated import proofs.
CREATE TABLE catabolic_imported_source_descriptors (
 source_id TEXT PRIMARY KEY REFERENCES catabolic_source_mappings(library_id),
 descriptor_json TEXT NOT NULL,
 proof_json TEXT NOT NULL,
 activation_digest TEXT NOT NULL,
 created_at INTEGER NOT NULL
);
CREATE TRIGGER catabolic_imported_source_immutable_update BEFORE UPDATE ON catabolic_imported_source_descriptors BEGIN SELECT RAISE(ABORT,'imported source proof is immutable'); END;
CREATE TRIGGER catabolic_imported_source_immutable_delete BEFORE DELETE ON catabolic_imported_source_descriptors BEGIN SELECT RAISE(ABORT,'imported source proof is retained'); END;
