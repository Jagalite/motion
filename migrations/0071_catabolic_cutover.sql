CREATE TABLE catabolic_cutover (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 import_id TEXT NOT NULL UNIQUE REFERENCES catabolic_import_snapshots(import_id),
 checkpoint_json TEXT NOT NULL CHECK(json_valid(checkpoint_json)),
 activation_envelope TEXT NOT NULL CHECK(json_valid(activation_envelope)),
 phase TEXT NOT NULL CHECK(phase IN ('validated','activated')),
 CHECK(json_extract(checkpoint_json,'$.import_id')=import_id),
 CHECK(json_extract(checkpoint_json,'$.phase')=phase)
);
CREATE TRIGGER catabolic_cutover_no_delete BEFORE DELETE ON catabolic_cutover
BEGIN SELECT RAISE(ABORT,'cutover checkpoint cannot be deleted'); END;
CREATE TRIGGER catabolic_cutover_immutable BEFORE UPDATE ON catabolic_cutover
WHEN old.import_id<>new.import_id OR old.activation_envelope<>new.activation_envelope
 OR json_remove(old.checkpoint_json,'$.phase')<>json_remove(new.checkpoint_json,'$.phase')
 OR (old.phase='activated' AND new.phase<>'activated')
BEGIN SELECT RAISE(ABORT,'cutover identity is immutable'); END;
