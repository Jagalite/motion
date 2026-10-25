-- Integration journal only. This does not activate a backend or migrate media.
-- Catabolic owns results; Motion durably retains the exact intent before dispatch.
CREATE TABLE catabolic_binding (
 id INTEGER PRIMARY KEY CHECK (id=1),
 identity_json TEXT NOT NULL CHECK (json_valid(identity_json))
);
CREATE TRIGGER catabolic_binding_no_update BEFORE UPDATE ON catabolic_binding BEGIN
 SELECT RAISE(ABORT, 'Catabolic rebinding requires explicit reconciliation'); END;
CREATE TRIGGER catabolic_binding_no_delete BEFORE DELETE ON catabolic_binding BEGIN
 SELECT RAISE(ABORT, 'Catabolic rebinding requires explicit reconciliation'); END;
CREATE TABLE catabolic_commands (
 operation_id TEXT PRIMARY KEY,
 envelope_json TEXT NOT NULL CHECK (json_valid(envelope_json)),
 created_at INTEGER NOT NULL
);
CREATE TRIGGER catabolic_commands_no_update BEFORE UPDATE ON catabolic_commands BEGIN
 SELECT RAISE(ABORT, 'Catabolic command intents are immutable'); END;
CREATE TRIGGER catabolic_commands_no_delete BEFORE DELETE ON catabolic_commands BEGIN
 SELECT RAISE(ABORT, 'Catabolic command intents cannot be recycled'); END;
