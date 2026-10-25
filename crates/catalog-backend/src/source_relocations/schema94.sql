-- Candidate relocation control owner. Registration is coordinated separately.
-- Admission commits before any native effect. The exact native envelope is a
-- separate immutable child so admission and payload digests have no cycle.
CREATE TABLE catabolic_source_relocation_admissions (
 operation_id TEXT PRIMARY KEY NOT NULL,
 source_id TEXT NOT NULL,
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 predecessor_json TEXT NOT NULL CHECK(json_valid(predecessor_json) AND json_type(predecessor_json)='object'),
 old_mapping_json TEXT NOT NULL CHECK(json_valid(old_mapping_json) AND json_type(old_mapping_json)='object'),
 new_mapping_json TEXT NOT NULL CHECK(json_valid(new_mapping_json) AND json_type(new_mapping_json)='object'),
 old_mapping_digest TEXT NOT NULL CHECK(length(old_mapping_digest)=64 AND old_mapping_digest NOT GLOB '*[^0-9a-f]*'),
 new_mapping_digest TEXT NOT NULL CHECK(length(new_mapping_digest)=64 AND new_mapping_digest NOT GLOB '*[^0-9a-f]*'),
 requester_json TEXT NOT NULL CHECK(json_valid(requester_json) AND json_type(requester_json)='object'),
 commitment_json TEXT NOT NULL CHECK(json_valid(commitment_json) AND json_type(commitment_json)='object'),
 admission_digest TEXT NOT NULL CHECK(length(admission_digest)=64 AND admission_digest NOT GLOB '*[^0-9a-f]*'),
 created_at INTEGER NOT NULL,
 CHECK(old_mapping_digest<>new_mapping_digest),
 CHECK(COALESCE(json_extract(predecessor_json,'$.source_id')=source_id
  AND json_extract(predecessor_json,'$.catalog_id')=catalog_id
  AND json_extract(predecessor_json,'$.incarnation')=incarnation
  AND json_extract(predecessor_json,'$.mapping_digest')=old_mapping_digest
  AND json_extract(predecessor_json,'$.state')='active',0))
);
CREATE TABLE catabolic_source_relocation_commands (
 operation_id TEXT PRIMARY KEY NOT NULL REFERENCES catabolic_source_relocation_admissions(operation_id),
 command_json TEXT NOT NULL CHECK(json_valid(command_json) AND json_type(command_json)='object'),
 command_digest TEXT NOT NULL CHECK(length(command_digest)=64 AND command_digest NOT GLOB '*[^0-9a-f]*'),
 created_at INTEGER NOT NULL
);
CREATE TABLE catabolic_source_relocation_acks (
 operation_id TEXT PRIMARY KEY NOT NULL REFERENCES catabolic_source_relocation_commands(operation_id),
 native_effect_json TEXT NOT NULL CHECK(json_valid(native_effect_json) AND json_type(native_effect_json)='object'),
 native_effect_digest TEXT NOT NULL CHECK(length(native_effect_digest)=64 AND native_effect_digest NOT GLOB '*[^0-9a-f]*'),
 successor_json TEXT NOT NULL CHECK(json_valid(successor_json) AND json_type(successor_json)='object'),
 created_at INTEGER NOT NULL,
 source_id TEXT GENERATED ALWAYS AS (json_extract(successor_json,'$.source_id')) STORED NOT NULL,
 epoch INTEGER GENERATED ALWAYS AS (json_extract(successor_json,'$.epoch')) STORED NOT NULL,
 CHECK(COALESCE(json_type(successor_json,'$.source_id')='text' AND json_type(successor_json,'$.epoch')='integer' AND epoch>0,0)),
 FOREIGN KEY(source_id,epoch) REFERENCES catabolic_source_lifecycle(source_id,epoch) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catabolic_source_relocation_admissions_insert BEFORE INSERT ON catabolic_source_relocation_admissions
 WHEN motion_source_relocation_admission_allowed(NEW.operation_id,NEW.source_id,NEW.catalog_id,NEW.incarnation,NEW.predecessor_json,NEW.old_mapping_json,NEW.new_mapping_json,NEW.old_mapping_digest,NEW.new_mapping_digest,NEW.requester_json,NEW.commitment_json,NEW.admission_digest,NEW.created_at) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'source relocation admission requires exact retained owner'); END;
CREATE TRIGGER catabolic_source_relocation_commands_insert BEFORE INSERT ON catabolic_source_relocation_commands
 WHEN motion_source_relocation_command_allowed(NEW.operation_id,NEW.command_json,NEW.command_digest,NEW.created_at) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'source relocation command requires exact retained owner'); END;
CREATE TRIGGER catabolic_source_relocation_acks_insert BEFORE INSERT ON catabolic_source_relocation_acks
 WHEN motion_source_relocation_ack_allowed(NEW.operation_id,NEW.native_effect_json,NEW.native_effect_digest,NEW.successor_json,NEW.created_at) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'source relocation ACK requires verified native effect'); END;
CREATE TRIGGER catabolic_source_relocation_admissions_update BEFORE UPDATE ON catabolic_source_relocation_admissions
 BEGIN SELECT RAISE(ABORT,'source relocation admission immutable'); END;
CREATE TRIGGER catabolic_source_relocation_admissions_delete BEFORE DELETE ON catabolic_source_relocation_admissions
 BEGIN SELECT RAISE(ABORT,'source relocation admission retained'); END;
CREATE TRIGGER catabolic_source_relocation_commands_update BEFORE UPDATE ON catabolic_source_relocation_commands
 BEGIN SELECT RAISE(ABORT,'source relocation command immutable'); END;
CREATE TRIGGER catabolic_source_relocation_commands_delete BEFORE DELETE ON catabolic_source_relocation_commands
 BEGIN SELECT RAISE(ABORT,'source relocation command retained'); END;
CREATE TRIGGER catabolic_source_relocation_acks_update BEFORE UPDATE ON catabolic_source_relocation_acks
 BEGIN SELECT RAISE(ABORT,'source relocation ACK immutable'); END;
CREATE TRIGGER catabolic_source_relocation_acks_delete BEFORE DELETE ON catabolic_source_relocation_acks
 BEGIN SELECT RAISE(ABORT,'source relocation ACK retained'); END;

-- Retain both original91 branches verbatim. The sole added active successor
-- requires the exact immutable94 chain already present in the same writer.
-- The existing lifecycle insertion capability remains mandatory independently.
DROP TRIGGER catabolic_source_lifecycle_chain;
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
  OR
  (NEW.epoch>0 AND NEW.state='active' AND EXISTS(
   SELECT 1 FROM catabolic_source_lifecycle old
   JOIN catabolic_source_relocation_admissions intent ON intent.operation_id=NEW.operation_id
   JOIN catabolic_source_relocation_commands command ON command.operation_id=intent.operation_id
   JOIN catabolic_source_relocation_acks ack ON ack.operation_id=command.operation_id
   WHERE old.source_id=NEW.source_id AND intent.source_id=NEW.source_id
   AND old.epoch=NEW.epoch-1 AND old.epoch=(SELECT max(epoch) FROM catabolic_source_lifecycle WHERE source_id=NEW.source_id)
   AND old.state='active' AND old.catalog_id=NEW.catalog_id AND old.incarnation=NEW.incarnation
   AND old.source_revision=NEW.source_revision-1 AND old.binding_revision=NEW.binding_revision-1
   AND old.proof_json=NEW.proof_json AND old.mapping_digest<>NEW.mapping_digest
   AND intent.catalog_id=NEW.catalog_id AND intent.incarnation=NEW.incarnation
   AND intent.old_mapping_digest=old.mapping_digest AND intent.new_mapping_digest=NEW.mapping_digest
   AND json_extract(intent.predecessor_json,'$.epoch')=old.epoch
   AND json_extract(intent.predecessor_json,'$.source_revision')=old.source_revision
   AND json_extract(intent.predecessor_json,'$.binding_revision')=old.binding_revision
   AND json_extract(ack.successor_json,'$.source_id')=NEW.source_id
   AND json_extract(ack.successor_json,'$.epoch')=NEW.epoch
   AND json_extract(ack.successor_json,'$.catalog_id')=NEW.catalog_id
   AND json_extract(ack.successor_json,'$.incarnation')=NEW.incarnation
   AND json_extract(ack.successor_json,'$.mapping_digest')=NEW.mapping_digest
   AND json_extract(ack.successor_json,'$.source_revision')=NEW.source_revision
   AND json_extract(ack.successor_json,'$.binding_revision')=NEW.binding_revision
   AND json_extract(ack.successor_json,'$.state')='active'))
 )
 BEGIN SELECT RAISE(ABORT,'source lifecycle succession differs'); END;
