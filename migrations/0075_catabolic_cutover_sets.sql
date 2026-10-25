CREATE TABLE catabolic_cutover_sets (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 root_import_id TEXT NOT NULL UNIQUE REFERENCES catabolic_export_plans(root_import_id),
 manifest_digest TEXT NOT NULL,
 manifest_json TEXT NOT NULL CHECK(json_valid(manifest_json)),
 freeze_revision INTEGER NOT NULL CHECK(freeze_revision>0),
 phase TEXT NOT NULL CHECK(phase IN ('prepared','activated'))
);
CREATE TABLE catabolic_cutover_parts (
 import_id TEXT PRIMARY KEY REFERENCES catabolic_export_plan_parts(import_id),
 ordinal INTEGER NOT NULL UNIQUE,
 checkpoint_json TEXT NOT NULL CHECK(json_valid(checkpoint_json)),
 activation_envelope TEXT NOT NULL CHECK(json_valid(activation_envelope)),
 phase TEXT NOT NULL CHECK(phase IN ('validated','activated')),
 CHECK(json_extract(checkpoint_json,'$.import_id')=import_id),
 CHECK(json_extract(checkpoint_json,'$.phase')=phase)
);
CREATE TRIGGER catabolic_cutover_sets_no_delete BEFORE DELETE ON catabolic_cutover_sets BEGIN SELECT RAISE(ABORT,'cutover set is immutable'); END;
CREATE TRIGGER catabolic_cutover_sets_immutable BEFORE UPDATE ON catabolic_cutover_sets
WHEN old.root_import_id<>new.root_import_id OR old.manifest_digest<>new.manifest_digest OR old.manifest_json<>new.manifest_json OR old.freeze_revision<>new.freeze_revision OR (old.phase='activated' AND new.phase<>'activated')
BEGIN SELECT RAISE(ABORT,'cutover set is immutable'); END;
CREATE TRIGGER catabolic_cutover_parts_no_delete BEFORE DELETE ON catabolic_cutover_parts BEGIN SELECT RAISE(ABORT,'cutover part is immutable'); END;
CREATE TRIGGER catabolic_cutover_parts_immutable BEFORE UPDATE ON catabolic_cutover_parts
WHEN old.import_id<>new.import_id OR old.ordinal<>new.ordinal OR old.activation_envelope<>new.activation_envelope OR json_remove(old.checkpoint_json,'$.phase')<>json_remove(new.checkpoint_json,'$.phase') OR (old.phase='activated' AND new.phase<>'activated')
BEGIN SELECT RAISE(ABORT,'cutover part is immutable'); END;
