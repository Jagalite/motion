CREATE TABLE catabolic_source_state (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 revision INTEGER NOT NULL CHECK(revision>=0),
 writes_allowed INTEGER NOT NULL CHECK(writes_allowed IN(0,1)),
 state_json TEXT NOT NULL CHECK(json_valid(state_json)),
 CHECK(json_extract(state_json,'$.revision')=revision),
 CHECK(json_extract(state_json,'$.mode.kind') IN('live','frozen','sealed')),
 CHECK(writes_allowed=(json_extract(state_json,'$.mode.kind')='live'))
);
INSERT INTO catabolic_source_state VALUES(1,0,1,'{"revision":0,"mode":{"kind":"live"},"used_migrations":{}}');
CREATE TRIGGER catabolic_source_state_no_delete BEFORE DELETE ON catabolic_source_state BEGIN SELECT RAISE(ABORT,'source admission state cannot be deleted'); END;
CREATE TABLE catabolic_source_events (
 revision INTEGER PRIMARY KEY,
 input_json TEXT NOT NULL CHECK(json_valid(input_json)),
 state_json TEXT NOT NULL CHECK(json_valid(state_json))
);
CREATE TRIGGER catabolic_source_events_no_update BEFORE UPDATE ON catabolic_source_events BEGIN SELECT RAISE(ABORT,'source events are immutable'); END;
CREATE TRIGGER catabolic_source_events_no_delete BEFORE DELETE ON catabolic_source_events BEGIN SELECT RAISE(ABORT,'source events are immutable'); END;
CREATE TABLE catabolic_source_guards (
 trigger_name TEXT PRIMARY KEY,
 table_name TEXT NOT NULL,
 trigger_sql TEXT NOT NULL
);
CREATE TRIGGER catabolic_source_guards_no_update BEFORE UPDATE ON catabolic_source_guards BEGIN SELECT RAISE(ABORT,'source guards are immutable'); END;
CREATE TRIGGER catabolic_source_guards_no_delete BEFORE DELETE ON catabolic_source_guards BEGIN SELECT RAISE(ABORT,'source guards are immutable'); END;
CREATE TABLE catabolic_export_freezes (
 import_id TEXT PRIMARY KEY REFERENCES catabolic_motion_exports(import_id),
 freeze_revision INTEGER NOT NULL CHECK(freeze_revision>0)
);
CREATE TRIGGER catabolic_export_freezes_no_update BEFORE UPDATE ON catabolic_export_freezes BEGIN SELECT RAISE(ABORT,'export freeze bindings are immutable'); END;
CREATE TRIGGER catabolic_export_freezes_no_delete BEFORE DELETE ON catabolic_export_freezes BEGIN SELECT RAISE(ABORT,'export freeze bindings are immutable'); END;
