-- New native command keys are permitted only after an actual synchronous
-- older-generation refusal. Original payloads/IDs and every predecessor remain
-- immutable; absence, timeout, expiry and restore uncertainty are not evidence.
CREATE TABLE catabolic_command_attempts (
 business_id TEXT NOT NULL,
 attempt INTEGER NOT NULL CHECK(attempt BETWEEN 1 AND 31),
 original_envelope_json TEXT NOT NULL CHECK(json_valid(original_envelope_json)),
 prior_envelope_json TEXT NOT NULL CHECK(json_valid(prior_envelope_json)),
 envelope_json TEXT NOT NULL UNIQUE CHECK(json_valid(envelope_json)),
 stale_witness_json TEXT NOT NULL CHECK(json_valid(stale_witness_json)),
 created_at INTEGER NOT NULL,
 PRIMARY KEY(business_id,attempt)
);
CREATE TRIGGER catabolic_command_attempts_no_update BEFORE UPDATE ON catabolic_command_attempts BEGIN SELECT RAISE(ABORT,'immutable native command attempt'); END;
CREATE TRIGGER catabolic_command_attempts_no_delete BEFORE DELETE ON catabolic_command_attempts BEGIN SELECT RAISE(ABORT,'immutable native command attempt'); END;
