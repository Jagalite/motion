-- Explicit Motion watch authority; native observation_sources remains the only
-- dirty-generation owner. Old implicit actorless watches are not auto-adopted.
CREATE TABLE catabolic_source_watch_definitions (
 id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 catalog_id TEXT NOT NULL CHECK(length(catalog_id)>0),
 incarnation TEXT NOT NULL CHECK(length(incarnation)>0),
 origin TEXT NOT NULL CHECK(origin='authorized_create'),
 library_id TEXT NOT NULL,
 source_id TEXT NOT NULL,
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 tombstoned INTEGER NOT NULL CHECK(tombstoned IN (0,1)),
 definition_json TEXT NOT NULL CHECK(json_valid(definition_json) AND json_type(definition_json)='object'),
 created_at INTEGER NOT NULL,
 PRIMARY KEY(id,revision),
 CHECK(COALESCE(json_extract(definition_json,'$.id')=id,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.revision')=revision,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.catalog_id')=catalog_id,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.incarnation')=incarnation,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.library_id')=library_id,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.source_id')=source_id,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.enabled')=enabled,0)),
 CHECK(COALESCE(json_extract(definition_json,'$.tombstoned')=tombstoned,0)),
 CHECK(COALESCE(json_type(definition_json,'$.lifecycle')='object',0)),
 CHECK(tombstoned=0 OR enabled=0)
);
CREATE TRIGGER catabolic_source_watch_definition_immutable BEFORE UPDATE ON catabolic_source_watch_definitions
 BEGIN SELECT RAISE(ABORT,'watch definition revision is immutable'); END;
CREATE TRIGGER catabolic_source_watch_definition_retained BEFORE DELETE ON catabolic_source_watch_definitions
 BEGIN SELECT RAISE(ABORT,'watch definition history is retained'); END;
CREATE TRIGGER catabolic_source_watch_definition_succession BEFORE INSERT ON catabolic_source_watch_definitions
 WHEN (NEW.revision=1 AND EXISTS(SELECT 1 FROM catabolic_source_watch_definitions WHERE id=NEW.id))
 OR (NEW.revision>1 AND NOT EXISTS(SELECT 1 FROM catabolic_source_watch_definitions d
 WHERE d.id=NEW.id AND d.revision=NEW.revision-1 AND d.tombstoned=0
 AND d.catalog_id=NEW.catalog_id AND d.incarnation=NEW.incarnation AND d.origin=NEW.origin
 AND d.revision=(SELECT max(revision) FROM catabolic_source_watch_definitions WHERE id=NEW.id)))
 BEGIN SELECT RAISE(ABORT,'invalid watch definition succession'); END;
CREATE TABLE catabolic_source_watch_admissions (
 watch_id TEXT NOT NULL,
 watch_revision INTEGER NOT NULL,
 source_id TEXT NOT NULL,
 native_dirty_generation INTEGER NOT NULL CHECK(native_dirty_generation>=0),
 request_id TEXT NOT NULL UNIQUE REFERENCES catabolic_scan_requests(id),
 accepted_scope_json TEXT NOT NULL CHECK(json_valid(accepted_scope_json) AND json_type(accepted_scope_json)='object'),
 created_at INTEGER NOT NULL,
 PRIMARY KEY(watch_id,watch_revision,native_dirty_generation),
 FOREIGN KEY(watch_id,watch_revision) REFERENCES catabolic_source_watch_definitions(id,revision)
);
CREATE TRIGGER catabolic_source_watch_admission_immutable BEFORE UPDATE ON catabolic_source_watch_admissions
 BEGIN SELECT RAISE(ABORT,'watch admission is immutable'); END;
CREATE TRIGGER catabolic_source_watch_admission_retained BEFORE DELETE ON catabolic_source_watch_admissions
 BEGIN SELECT RAISE(ABORT,'watch admission history is retained'); END;
