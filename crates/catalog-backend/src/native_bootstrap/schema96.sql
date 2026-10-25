-- Explicit40→41 transition and bootstrap journal. These rows do not modify the
-- original binding or its separate39→40 acknowledgment. A bootstrap command is
-- saved only after actual native migration, before its first effect dispatch.
CREATE TABLE catabolic_native_bootstrap_intents (
 transition_id TEXT NOT NULL PRIMARY KEY,
 intent_json TEXT NOT NULL CHECK(json_valid(intent_json)),
 CHECK(json_extract(intent_json,'$.contract_version') IS 1),
 CHECK(json_extract(intent_json,'$.transition_id') IS transition_id),
 CHECK(json_extract(intent_json,'$.from.schema_version') IS 40),
 CHECK(json_extract(intent_json,'$.to.schema_version') IS 41)
);
CREATE TABLE catabolic_native_bootstrap_commands (
 transition_id TEXT NOT NULL PRIMARY KEY REFERENCES catabolic_native_bootstrap_intents(transition_id),
 command_json TEXT NOT NULL CHECK(json_valid(command_json))
);
CREATE TABLE catabolic_native_bootstrap_acks (
 transition_id TEXT NOT NULL PRIMARY KEY REFERENCES catabolic_native_bootstrap_commands(transition_id),
 proof_json TEXT NOT NULL CHECK(json_valid(proof_json)),
 CHECK(json_extract(proof_json,'$.contract_version') IS 1),
 CHECK(json_extract(proof_json,'$.transition_id') IS transition_id)
);
CREATE TRIGGER catabolic_native_bootstrap_intents_insert BEFORE INSERT ON catabolic_native_bootstrap_intents
 WHEN motion_native_bootstrap_insert('intent',NEW.transition_id,NEW.intent_json) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'native41 intent requires retained offline owner'); END;
CREATE TRIGGER catabolic_native_bootstrap_commands_insert BEFORE INSERT ON catabolic_native_bootstrap_commands
 WHEN motion_native_bootstrap_insert('command',NEW.transition_id,NEW.command_json) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'native41 command requires exact migrated owner'); END;
CREATE TRIGGER catabolic_native_bootstrap_acks_insert BEFORE INSERT ON catabolic_native_bootstrap_acks
 WHEN motion_native_bootstrap_insert('ack',NEW.transition_id,NEW.proof_json) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'native41 ACK requires measured Complete owner'); END;
CREATE TRIGGER catabolic_native_bootstrap_intents_update BEFORE UPDATE ON catabolic_native_bootstrap_intents BEGIN SELECT RAISE(ABORT,'native41 intent is immutable'); END;
CREATE TRIGGER catabolic_native_bootstrap_intents_delete BEFORE DELETE ON catabolic_native_bootstrap_intents BEGIN SELECT RAISE(ABORT,'native41 intent is retained'); END;
CREATE TRIGGER catabolic_native_bootstrap_commands_update BEFORE UPDATE ON catabolic_native_bootstrap_commands BEGIN SELECT RAISE(ABORT,'native41 command is immutable'); END;
CREATE TRIGGER catabolic_native_bootstrap_commands_delete BEFORE DELETE ON catabolic_native_bootstrap_commands BEGIN SELECT RAISE(ABORT,'native41 command is retained'); END;
CREATE TRIGGER catabolic_native_bootstrap_acks_update BEFORE UPDATE ON catabolic_native_bootstrap_acks BEGIN SELECT RAISE(ABORT,'native41 ACK is immutable'); END;
CREATE TRIGGER catabolic_native_bootstrap_acks_delete BEFORE DELETE ON catabolic_native_bootstrap_acks BEGIN SELECT RAISE(ABORT,'native41 ACK is retained'); END;
