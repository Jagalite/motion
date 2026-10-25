-- Original binding and activation receipts remain immutable. This singleton is
-- the one reviewed native39→40 transition, not a general schema acceptance flag.
CREATE TABLE catabolic_native_upgrade_intent (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 intent_json TEXT NOT NULL CHECK(json_valid(intent_json))
 CHECK(json_extract(intent_json,'$.contract_version') IS 1)
 CHECK(json_extract(intent_json,'$.from.schema_version') IS 39)
 CHECK(json_extract(intent_json,'$.to.schema_version') IS 40)
);
CREATE TABLE catabolic_native_upgrade_ack (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1) REFERENCES catabolic_native_upgrade_intent(singleton),
 proof_json TEXT NOT NULL CHECK(json_valid(proof_json))
 CHECK(json_extract(proof_json,'$.contract_version') IS 1)
 CHECK(json_extract(proof_json,'$.from_schema') IS 39)
 CHECK(json_extract(proof_json,'$.to_schema') IS 40)
);
CREATE TRIGGER catabolic_native_upgrade_intent_insert BEFORE INSERT ON catabolic_native_upgrade_intent
 WHEN motion_native_upgrade('intent',NEW.intent_json) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'native upgrade intent requires isolated operator owner'); END;
CREATE TRIGGER catabolic_native_upgrade_ack_insert BEFORE INSERT ON catabolic_native_upgrade_ack
 WHEN motion_native_upgrade('ack',NEW.proof_json) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'native upgrade ACK requires exact retained proof owner'); END;
CREATE TRIGGER catabolic_native_upgrade_intent_update BEFORE UPDATE ON catabolic_native_upgrade_intent
 BEGIN SELECT RAISE(ABORT,'native upgrade intent is immutable'); END;
CREATE TRIGGER catabolic_native_upgrade_intent_delete BEFORE DELETE ON catabolic_native_upgrade_intent
 BEGIN SELECT RAISE(ABORT,'native upgrade intent is retained'); END;
CREATE TRIGGER catabolic_native_upgrade_ack_update BEFORE UPDATE ON catabolic_native_upgrade_ack
 BEGIN SELECT RAISE(ABORT,'native upgrade ACK is immutable'); END;
CREATE TRIGGER catabolic_native_upgrade_ack_delete BEFORE DELETE ON catabolic_native_upgrade_ack
 BEGIN SELECT RAISE(ABORT,'native upgrade ACK is retained'); END;
