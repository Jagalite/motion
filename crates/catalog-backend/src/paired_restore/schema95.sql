-- Append-only identity ancestry for a verified paired restore. This never
-- rewrites frozen activation, business envelopes, attempts or native receipts.
-- Filesystem pair-install recovery is a separate retained owner; these rows
-- alone do not authorize publication, repair, dispatch or cleanup.
CREATE TABLE catabolic_paired_restore_intents (
 transition_id TEXT NOT NULL PRIMARY KEY,
 intent_json TEXT NOT NULL CHECK(json_valid(intent_json)),
 CHECK(json_extract(intent_json,'$.contract_version') IS 1),
 CHECK(json_extract(intent_json,'$.transition_id') IS transition_id),
 CHECK(json_extract(intent_json,'$.from.catalog_id') IS json_extract(intent_json,'$.to.catalog_id')),
 CHECK(json_extract(intent_json,'$.from.schema_version') IS json_extract(intent_json,'$.to.schema_version')),
 CHECK(json_extract(intent_json,'$.from.incarnation') IS NOT json_extract(intent_json,'$.to.incarnation'))
);
CREATE TABLE catabolic_paired_restore_acks (
 transition_id TEXT NOT NULL PRIMARY KEY REFERENCES catabolic_paired_restore_intents(transition_id),
 proof_json TEXT NOT NULL CHECK(json_valid(proof_json)),
 CHECK(json_extract(proof_json,'$.contract_version') IS 1),
 CHECK(json_extract(proof_json,'$.transition_id') IS transition_id)
);
CREATE TRIGGER catabolic_paired_restore_intents_insert BEFORE INSERT ON catabolic_paired_restore_intents
 WHEN motion_paired_restore_insert('intent',NEW.transition_id,NEW.intent_json) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'paired restore intent requires isolated exact owner'); END;
CREATE TRIGGER catabolic_paired_restore_acks_insert BEFORE INSERT ON catabolic_paired_restore_acks
 WHEN motion_paired_restore_insert('ack',NEW.transition_id,NEW.proof_json) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'paired restore ACK requires exact staged native lineage'); END;
CREATE TRIGGER catabolic_paired_restore_intents_update BEFORE UPDATE ON catabolic_paired_restore_intents
 BEGIN SELECT RAISE(ABORT,'paired restore intent is immutable'); END;
CREATE TRIGGER catabolic_paired_restore_intents_delete BEFORE DELETE ON catabolic_paired_restore_intents
 BEGIN SELECT RAISE(ABORT,'paired restore intent is retained'); END;
CREATE TRIGGER catabolic_paired_restore_acks_update BEFORE UPDATE ON catabolic_paired_restore_acks
 BEGIN SELECT RAISE(ABORT,'paired restore ACK is immutable'); END;
CREATE TRIGGER catabolic_paired_restore_acks_delete BEFORE DELETE ON catabolic_paired_restore_acks
 BEGIN SELECT RAISE(ABORT,'paired restore ACK is retained'); END;
