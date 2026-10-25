-- Explicit deployment/access withdrawal. Native observations and identity remain
-- immutable history; absence is never an active-state grant.
CREATE TABLE catabolic_source_lifecycle (
 source_id TEXT NOT NULL,
 epoch INTEGER NOT NULL CHECK(epoch>=0),
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 mapping_digest TEXT NOT NULL CHECK(length(mapping_digest)=64),
 source_revision INTEGER NOT NULL CHECK(source_revision>=0),
 binding_revision INTEGER NOT NULL CHECK(binding_revision>=0),
 state TEXT NOT NULL CHECK(state IN ('active','withdrawn','tombstoned')),
 operation_id TEXT NOT NULL UNIQUE,
 proof_json TEXT NOT NULL CHECK(json_valid(proof_json)),
 created_at INTEGER NOT NULL,
 PRIMARY KEY(source_id,epoch)
);
CREATE TRIGGER catabolic_source_lifecycle_insert BEFORE INSERT ON catabolic_source_lifecycle
 WHEN motion_source_lifecycle_insert_allowed(NEW.source_id,NEW.epoch,NEW.catalog_id,NEW.incarnation,NEW.mapping_digest,NEW.source_revision,NEW.binding_revision,NEW.state,NEW.operation_id,NEW.proof_json,NEW.created_at)<>1
 BEGIN SELECT RAISE(ABORT,'source lifecycle requires exact retained owner'); END;
CREATE TRIGGER catabolic_source_lifecycle_chain BEFORE INSERT ON catabolic_source_lifecycle
 WHEN NOT (
  (NEW.epoch=0 AND NEW.state='active' AND NOT EXISTS(SELECT 1 FROM catabolic_source_lifecycle WHERE source_id=NEW.source_id))
  OR
  (NEW.epoch>0 AND NEW.state IN ('withdrawn','tombstoned') AND EXISTS(
   SELECT 1 FROM catabolic_source_lifecycle old WHERE old.source_id=NEW.source_id
   AND old.epoch=NEW.epoch-1 AND old.epoch=(SELECT max(epoch) FROM catabolic_source_lifecycle WHERE source_id=NEW.source_id)
   AND old.state<>'tombstoned' AND old.catalog_id=NEW.catalog_id AND old.incarnation=NEW.incarnation
   AND old.mapping_digest=NEW.mapping_digest AND old.binding_revision=NEW.binding_revision
   AND old.source_revision=NEW.source_revision-1 AND old.proof_json=NEW.proof_json))
 )
 BEGIN SELECT RAISE(ABORT,'source lifecycle succession differs'); END;
CREATE TRIGGER catabolic_source_lifecycle_update BEFORE UPDATE ON catabolic_source_lifecycle
 BEGIN SELECT RAISE(ABORT,'source lifecycle history is immutable'); END;
CREATE TRIGGER catabolic_source_lifecycle_delete BEFORE DELETE ON catabolic_source_lifecycle
 BEGIN SELECT RAISE(ABORT,'source lifecycle history is retained'); END;
-- Generated membership projection is appended by the projection owner.

-- Generated output scope is a distinct receipt-backed proof domain. It never
-- weakens or rewrites original occurrence membership contract 1.
CREATE TABLE catabolic_projection_generated_libraries (
 file_id TEXT NOT NULL,
 library_id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation>=0),
 proof_json TEXT NOT NULL CHECK(json_valid(proof_json) AND json_extract(proof_json,'$.contract_version')=2 AND json_extract(proof_json,'$.kind')='generated'),
 PRIMARY KEY(file_id,library_id)
);
CREATE TRIGGER catabolic_projection_generated_libraries_insert BEFORE INSERT ON catabolic_projection_generated_libraries
 WHEN motion_native_projection('catabolic_projection_generated_libraries')<>1
 BEGIN SELECT RAISE(ABORT,'generated library projection requires isolated owner'); END;
CREATE TRIGGER catabolic_projection_generated_libraries_update BEFORE UPDATE ON catabolic_projection_generated_libraries
 WHEN motion_native_projection('catabolic_projection_generated_libraries')<>1
 BEGIN SELECT RAISE(ABORT,'generated library projection requires isolated owner'); END;
CREATE TRIGGER catabolic_projection_generated_libraries_delete BEFORE DELETE ON catabolic_projection_generated_libraries
 WHEN motion_native_projection('catabolic_projection_generated_libraries')<>1
 BEGIN SELECT RAISE(ABORT,'generated library projection requires isolated owner'); END;
