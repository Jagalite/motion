-- Host-assigned IDs and probes survive retries before native effects dispatch.
CREATE TABLE catabolic_discovery_intents (
 operation_id TEXT PRIMARY KEY,
 source_id TEXT NOT NULL,
 native_profile TEXT NOT NULL,
 native_file_id TEXT NOT NULL,
 native_revision TEXT NOT NULL,
 identity_json TEXT NOT NULL,
 ids_json TEXT NOT NULL,
 observed_json TEXT NOT NULL,
 probe_json TEXT,
 command_json TEXT,
 receipt_json TEXT,
 state TEXT NOT NULL CHECK(state IN ('planned','probed','committed')),
 created_at INTEGER NOT NULL,
 UNIQUE(native_profile,native_file_id,native_revision)
);
CREATE TRIGGER catabolic_discovery_identity_immutable BEFORE UPDATE ON catabolic_discovery_intents
 WHEN NEW.operation_id IS NOT OLD.operation_id OR NEW.source_id IS NOT OLD.source_id
 OR NEW.native_profile IS NOT OLD.native_profile OR NEW.native_file_id IS NOT OLD.native_file_id
 OR NEW.native_revision IS NOT OLD.native_revision OR NEW.identity_json IS NOT OLD.identity_json
 OR NEW.ids_json IS NOT OLD.ids_json OR NEW.observed_json IS NOT OLD.observed_json
 BEGIN SELECT RAISE(ABORT,'native discovery identity is immutable'); END;
CREATE TRIGGER catabolic_discovery_probe_immutable BEFORE UPDATE ON catabolic_discovery_intents
 WHEN (OLD.probe_json IS NOT NULL AND NEW.probe_json IS NOT OLD.probe_json)
 OR (OLD.command_json IS NOT NULL AND NEW.command_json IS NOT OLD.command_json)
 OR (OLD.receipt_json IS NOT NULL AND NEW.receipt_json IS NOT OLD.receipt_json)
 OR (OLD.state='probed' AND NEW.state='planned')
 OR (OLD.state='committed' AND NEW.state!='committed')
 BEGIN SELECT RAISE(ABORT,'native discovery proof and intent are immutable'); END;
CREATE TRIGGER catabolic_discovery_no_delete BEFORE DELETE ON catabolic_discovery_intents
 BEGIN SELECT RAISE(ABORT,'native discovery intents are retained'); END;
