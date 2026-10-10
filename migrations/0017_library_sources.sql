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

-- Event identities follow the logical catalog too. Preserve the prior logical
-- membership in each hint so delayed readers can invalidate a removed item
-- without receiving its now-hidden identity. Physical roots are administrative.
DROP TRIGGER event_libraries_insert;
DROP TRIGGER event_libraries_update;
DROP TRIGGER event_libraries_delete;
CREATE TRIGGER event_libraries_insert AFTER INSERT ON libraries BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('sources',NEW.id); END;
CREATE TRIGGER event_libraries_update AFTER UPDATE ON libraries BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('sources',NEW.id);
 INSERT INTO change_events(topic,resource_id) SELECT 'libraries',library_id FROM library_sources WHERE source_id=NEW.id;
END;
CREATE TRIGGER event_libraries_delete AFTER DELETE ON libraries BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('sources',OLD.id); END;
DROP TRIGGER event_media_files_insert;
DROP TRIGGER event_media_files_update;
DROP TRIGGER event_media_files_delete;
CREATE TRIGGER event_media_files_insert AFTER INSERT ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=NEW.library_id WHERE e.id=NEW.edition_id;
END;
CREATE TRIGGER event_media_files_update AFTER UPDATE ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=OLD.library_id WHERE e.id=OLD.edition_id;
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=NEW.library_id WHERE e.id=NEW.edition_id AND (NEW.edition_id!=OLD.edition_id OR NEW.library_id!=OLD.library_id);
END;
CREATE TRIGGER event_media_files_delete AFTER DELETE ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=OLD.library_id WHERE e.id=OLD.edition_id;
END;
