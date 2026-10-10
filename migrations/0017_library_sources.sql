-- Logical libraries over registered storage sources. The existing `libraries`
-- rows are the physical sources (root, volume identity, binding revision,
-- enablement); media_files, jobs and schedules keep referencing them. Logical
-- libraries carry kind/language policy and reference one or more sources.
ALTER TABLE libraries ADD COLUMN exclusions_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE jobs ADD COLUMN binding_revision INTEGER;

CREATE TABLE catalog_libraries (
 id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision >= 1),
 name TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('movies','television','music','photos','personal_video','mixed')),
 language TEXT NOT NULL DEFAULT ''
);
CREATE TABLE library_sources (
 library_id TEXT NOT NULL REFERENCES catalog_libraries(id) ON DELETE CASCADE,
 source_id TEXT NOT NULL REFERENCES libraries(id),
 PRIMARY KEY(library_id,source_id)
);
CREATE INDEX library_sources_source ON library_sources(source_id,library_id);
-- Each existing root becomes one mixed library with the same ID, so old IDs
-- map deterministically and nothing is rescanned as a duplicate.
INSERT INTO catalog_libraries (id,name,kind) SELECT id,name,'mixed' FROM libraries WHERE managed=0;
INSERT INTO library_sources SELECT id,id FROM libraries WHERE managed=0;

-- Contract-shaped view of the physical sources (managed output roots excluded).
CREATE VIEW sources AS
 SELECT id,revision,binding_revision,name,root AS root_path,root_identity AS volume_identity,enabled,exclusions_json
 FROM libraries WHERE managed=0;

CREATE TRIGGER event_catalog_libraries_insert AFTER INSERT ON catalog_libraries BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('libraries',NEW.id); END;
CREATE TRIGGER event_catalog_libraries_update AFTER UPDATE ON catalog_libraries BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('libraries',NEW.id); END;
CREATE TRIGGER event_catalog_libraries_delete AFTER DELETE ON catalog_libraries BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('libraries',OLD.id); END;
CREATE TRIGGER event_library_sources_insert AFTER INSERT ON library_sources BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('libraries',NEW.library_id); END;
CREATE TRIGGER event_library_sources_delete AFTER DELETE ON library_sources BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('libraries',OLD.library_id); END;
