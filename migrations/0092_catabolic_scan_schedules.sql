-- Motion-owned scan schedule companion. Legacy schedules, foreign keys and
-- progress remain unchanged; adoption and execution require current owner proof.
CREATE TABLE catabolic_scan_schedule_adoptions (
 schedule_id TEXT PRIMARY KEY,
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 original_json TEXT NOT NULL CHECK(json_valid(original_json) AND json_type(original_json)='object'),
 progress_json TEXT NOT NULL CHECK(json_valid(progress_json)),
 created_at INTEGER NOT NULL,
 CHECK(COALESCE(json_extract(original_json,'$.id')=schedule_id,0))
);
CREATE TABLE catabolic_scan_schedule_definitions (
 id TEXT PRIMARY KEY,
 catalog_id TEXT NOT NULL CHECK(length(catalog_id)>0),
 incarnation TEXT NOT NULL CHECK(length(incarnation)>0),
 origin TEXT NOT NULL CHECK(origin IN ('legacy_adoption','native_create')),
 revision INTEGER NOT NULL CHECK(revision>0),
 library_id TEXT NOT NULL,
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 tombstoned INTEGER NOT NULL DEFAULT 0 CHECK(tombstoned IN (0,1)),
 definition_json TEXT NOT NULL CHECK(json_valid(definition_json) AND json_type(definition_json)='object'),
 adoption_id TEXT UNIQUE REFERENCES catabolic_scan_schedule_adoptions(schedule_id),
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 CHECK(COALESCE(json_extract(definition_json,'$.id')=id,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.revision')=revision,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.library_id')=library_id,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.enabled')=enabled,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.task')='scan',0)),
 CHECK(tombstoned=0 OR enabled=0),
 CHECK((origin='legacy_adoption' AND adoption_id IS NOT NULL) OR (origin='native_create' AND adoption_id IS NULL))
);
CREATE TABLE catabolic_scan_schedule_cursors (
 schedule_id TEXT PRIMARY KEY REFERENCES catabolic_scan_schedule_definitions(id),
 definition_revision INTEGER NOT NULL CHECK(definition_revision>0),
 due_at INTEGER NOT NULL,
 retry_at INTEGER NOT NULL CHECK(retry_at>=due_at)
);
CREATE TABLE catabolic_scan_schedule_occurrences (
 schedule_id TEXT NOT NULL REFERENCES catabolic_scan_schedule_definitions(id),
 definition_revision INTEGER NOT NULL CHECK(definition_revision>0),
 due_at INTEGER NOT NULL,
 request_id TEXT NOT NULL UNIQUE REFERENCES catabolic_scan_requests(id),
 accepted_scope_json TEXT NOT NULL CHECK(json_valid(accepted_scope_json) AND json_type(accepted_scope_json)='object'),
 created_at INTEGER NOT NULL,
 PRIMARY KEY(schedule_id,definition_revision,due_at)
);
CREATE TRIGGER catabolic_schedule_adoption_immutable BEFORE UPDATE ON catabolic_scan_schedule_adoptions
 BEGIN SELECT RAISE(ABORT,'schedule adoption is immutable'); END;
CREATE TRIGGER catabolic_schedule_adoption_retained BEFORE DELETE ON catabolic_scan_schedule_adoptions
 BEGIN SELECT RAISE(ABORT,'schedule adoption is retained'); END;
CREATE TRIGGER catabolic_schedule_occurrence_immutable BEFORE UPDATE ON catabolic_scan_schedule_occurrences
 BEGIN SELECT RAISE(ABORT,'schedule occurrence is immutable'); END;
CREATE TRIGGER catabolic_schedule_occurrence_retained BEFORE DELETE ON catabolic_scan_schedule_occurrences
 BEGIN SELECT RAISE(ABORT,'schedule occurrence is retained'); END;
CREATE TRIGGER catabolic_schedule_definition_retained BEFORE DELETE ON catabolic_scan_schedule_definitions
 BEGIN SELECT RAISE(ABORT,'schedule definitions require tombstones'); END;
CREATE TRIGGER catabolic_schedule_definition_succession BEFORE UPDATE ON catabolic_scan_schedule_definitions
 WHEN NEW.id IS NOT OLD.id OR NEW.adoption_id IS NOT OLD.adoption_id
 OR NEW.catalog_id IS NOT OLD.catalog_id OR NEW.incarnation IS NOT OLD.incarnation OR NEW.origin IS NOT OLD.origin
 OR NEW.created_at IS NOT OLD.created_at OR NEW.revision IS NOT OLD.revision+1
 OR OLD.tombstoned=1
 BEGIN SELECT RAISE(ABORT,'invalid schedule definition succession'); END;
CREATE TRIGGER catabolic_schedule_cursor_retained BEFORE DELETE ON catabolic_scan_schedule_cursors
 BEGIN SELECT RAISE(ABORT,'schedule cursor is retained'); END;
