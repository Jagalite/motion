-- Durable pre-effect request identity; native source effects may commit before
-- the host acknowledgment. Never accept a different request on replay.
CREATE TABLE catabolic_source_registration_intents (
    operation_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL UNIQUE,
    request_digest TEXT NOT NULL,
    descriptor_json TEXT NOT NULL CHECK(json_valid(descriptor_json)),
    created_at INTEGER NOT NULL
);
CREATE TRIGGER catabolic_source_registration_intents_immutable_update
BEFORE UPDATE ON catabolic_source_registration_intents BEGIN SELECT RAISE(ABORT,'immutable native source registration intent'); END;
CREATE TRIGGER catabolic_source_registration_intents_immutable_delete
BEFORE DELETE ON catabolic_source_registration_intents BEGIN SELECT RAISE(ABORT,'immutable native source registration intent'); END;
