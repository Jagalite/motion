-- Native history keeps immutable real locators and FK identities. This entire
-- script AND SQLx bookkeeping run in one transaction. No foreign_keys=OFF.
-- Deferred DROP bookkeeping is reset only AFTER enforced full FK/row checks.
CREATE TEMP TABLE motion_rebuild_assert(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO motion_rebuild_assert VALUES(((SELECT foreign_keys FROM pragma_foreign_keys)=1));
INSERT INTO motion_rebuild_assert VALUES((NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check)));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='index' AND name='media_files_revision')='CREATE INDEX media_files_revision ON media_files(revision)'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='table' AND name='media_files')='CREATE TABLE media_files (
 id TEXT PRIMARY KEY, edition_id TEXT NOT NULL REFERENCES editions(id), library_id TEXT NOT NULL REFERENCES libraries(id), relative_path TEXT NOT NULL,
 revision TEXT NOT NULL, fingerprint TEXT NOT NULL, bytes INTEGER NOT NULL,
 duration_seconds REAL, tracks_json TEXT NOT NULL DEFAULT ''[]'', available INTEGER NOT NULL DEFAULT 1, generated INTEGER NOT NULL DEFAULT 0, chapters_json TEXT,
 UNIQUE(library_id, relative_path)
)'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='catalog_revision_file_change')='CREATE TRIGGER catalog_revision_file_change AFTER UPDATE OF edition_id,revision ON media_files
 WHEN OLD.edition_id IS NOT NEW.edition_id OR OLD.revision IS NOT NEW.revision BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (SELECT item_id FROM editions WHERE id IN (OLD.edition_id,NEW.edition_id)); END'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='catalog_revision_file_delete')='CREATE TRIGGER catalog_revision_file_delete AFTER DELETE ON media_files BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT item_id FROM editions WHERE id=OLD.edition_id); END'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='catalog_revision_file_insert')='CREATE TRIGGER catalog_revision_file_insert AFTER INSERT ON media_files BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT item_id FROM editions WHERE id=NEW.edition_id); END'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='event_media_files_delete')='CREATE TRIGGER event_media_files_delete AFTER DELETE ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT ''catalog'',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=OLD.library_id WHERE e.id=OLD.edition_id;
END'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='event_media_files_insert')='CREATE TRIGGER event_media_files_insert AFTER INSERT ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT ''catalog'',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=NEW.library_id WHERE e.id=NEW.edition_id;
END'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='event_media_files_update')='CREATE TRIGGER event_media_files_update AFTER UPDATE ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT ''catalog'',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=OLD.library_id WHERE e.id=OLD.edition_id;
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT ''catalog'',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=NEW.library_id WHERE e.id=NEW.edition_id AND (NEW.edition_id!=OLD.edition_id OR NEW.library_id!=OLD.library_id);
END'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='preparation_file_update')='CREATE TRIGGER preparation_file_update AFTER UPDATE OF revision,available,library_id,edition_id ON media_files
WHEN OLD.revision IS NOT NEW.revision OR OLD.available IS NOT NEW.available OR OLD.library_id IS NOT NEW.library_id OR OLD.edition_id IS NOT NEW.edition_id BEGIN
 UPDATE preparations SET revision=revision+1 WHERE job_id IN (SELECT id FROM processing_jobs WHERE source_file_id=NEW.id OR output_file_id=NEW.id);
END'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE type='view' AND name='catalog_files')='CREATE VIEW catalog_files AS SELECT f.*,e.item_id,e.label AS edition_label,i.title,i.kind
 FROM media_files f JOIN editions e ON e.id=f.edition_id JOIN items i ON i.id=e.item_id'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name='media_files' AND name NOT LIKE 'motion_source_freeze_%')=7));
INSERT INTO motion_rebuild_assert VALUES(((SELECT count(*) FROM sqlite_schema WHERE type='index' AND tbl_name='media_files' AND sql IS NOT NULL)=1));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE name='match_proposals')='CREATE TABLE match_proposals (
 id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL CHECK(revision >= 1),
 file_id TEXT NOT NULL REFERENCES media_files(id),
 file_revision TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN (''pending'',''review'',''accepted'',''rejected'',''deferred'',''stale'')),
 candidates_json TEXT NOT NULL,
 decision_json TEXT,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL
)'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE name='nfo_origins')='CREATE TABLE nfo_origins (
 item_id TEXT PRIMARY KEY REFERENCES items(id),
 file_id TEXT NOT NULL REFERENCES media_files(id)
)'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE name='playback_sessions')='CREATE TABLE playback_sessions (
 id TEXT PRIMARY KEY, profile_id TEXT NOT NULL REFERENCES profiles(id), item_id TEXT NOT NULL REFERENCES items(id),
 file_id TEXT NOT NULL REFERENCES media_files(id), file_revision TEXT NOT NULL, duration_seconds REAL,
 sequence INTEGER NOT NULL DEFAULT 0, position_seconds REAL NOT NULL, status TEXT NOT NULL,
 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE name='processing_jobs')='CREATE TABLE processing_jobs (
 id TEXT PRIMARY KEY, source_file_id TEXT NOT NULL REFERENCES media_files(id), source_revision TEXT NOT NULL,
 recipe TEXT NOT NULL, backend TEXT NOT NULL, idempotency_key TEXT NOT NULL UNIQUE,
 phase TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0, progress_seconds REAL NOT NULL DEFAULT 0,
 output_file_id TEXT REFERENCES media_files(id), error TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
 expired INTEGER NOT NULL DEFAULT 0
, cache_cleaned INTEGER NOT NULL DEFAULT 0, video_profile TEXT, requester_id TEXT NOT NULL DEFAULT ''legacy'', revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>=1))'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE name='renditions')='CREATE TABLE renditions (
 id TEXT PRIMARY KEY, item_id TEXT NOT NULL REFERENCES items(id), source TEXT NOT NULL, external_id TEXT NOT NULL,
 file_id TEXT NOT NULL REFERENCES media_files(id), file_revision TEXT NOT NULL,
 source_file_id TEXT NOT NULL REFERENCES media_files(id), source_revision TEXT NOT NULL,
 label TEXT NOT NULL, recipe_json TEXT NOT NULL, updated_at INTEGER NOT NULL,
 UNIQUE(source,external_id)
)'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE name='sidecar_subtitles')='CREATE TABLE sidecar_subtitles (
 file_id TEXT NOT NULL REFERENCES media_files(id),
 name TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 language TEXT,
 forced INTEGER NOT NULL CHECK(forced IN (0,1)),
 hearing_impaired INTEGER NOT NULL CHECK(hearing_impaired IN (0,1)),
 format TEXT NOT NULL CHECK(format IN (''srt'',''vtt'',''ass'',''ssa'')),
 PRIMARY KEY(file_id,name)
)'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT sql FROM sqlite_schema WHERE name='version_files')='CREATE TABLE version_files (
 version_id TEXT NOT NULL REFERENCES media_versions(id) ON DELETE CASCADE,
 part INTEGER NOT NULL CHECK(part >= 1),
 file_id TEXT NOT NULL REFERENCES media_files(id),
 file_revision TEXT NOT NULL,
 start_ms INTEGER CHECK(start_ms IS NULL OR start_ms >= 0),
 end_ms INTEGER CHECK(end_ms IS NULL OR end_ms >= 0),
 CHECK (start_ms IS NULL OR end_ms IS NULL OR start_ms < end_ms),
 PRIMARY KEY(version_id,part),
 UNIQUE(version_id,file_id)
)'));
INSERT INTO motion_rebuild_assert VALUES(((SELECT count(*) FROM sqlite_schema m JOIN pragma_foreign_key_list(m.name) f WHERE m.type='table' AND f."table"='media_files')=9 AND NOT EXISTS(SELECT 1 FROM sqlite_schema m JOIN pragma_foreign_key_list(m.name) f WHERE m.type='table' AND f."table"='media_files' AND NOT ((m.name='renditions' AND f.id=0 AND f.seq=0 AND f."from"='source_file_id' AND f."to"='id' AND f.on_update='NO ACTION' AND f.on_delete='NO ACTION' AND f.match='NONE') OR (m.name='renditions' AND f.id=1 AND f.seq=0 AND f."from"='file_id' AND f."to"='id' AND f.on_update='NO ACTION' AND f.on_delete='NO ACTION' AND f.match='NONE') OR (m.name='playback_sessions' AND f.id=0 AND f.seq=0 AND f."from"='file_id' AND f."to"='id' AND f.on_update='NO ACTION' AND f.on_delete='NO ACTION' AND f.match='NONE') OR (m.name='processing_jobs' AND f.id=0 AND f.seq=0 AND f."from"='output_file_id' AND f."to"='id' AND f.on_update='NO ACTION' AND f.on_delete='NO ACTION' AND f.match='NONE') OR (m.name='processing_jobs' AND f.id=1 AND f.seq=0 AND f."from"='source_file_id' AND f."to"='id' AND f.on_update='NO ACTION' AND f.on_delete='NO ACTION' AND f.match='NONE') OR (m.name='match_proposals' AND f.id=0 AND f.seq=0 AND f."from"='file_id' AND f."to"='id' AND f.on_update='NO ACTION' AND f.on_delete='NO ACTION' AND f.match='NONE') OR (m.name='version_files' AND f.id=0 AND f.seq=0 AND f."from"='file_id' AND f."to"='id' AND f.on_update='NO ACTION' AND f.on_delete='NO ACTION' AND f.match='NONE') OR (m.name='nfo_origins' AND f.id=0 AND f.seq=0 AND f."from"='file_id' AND f."to"='id' AND f.on_update='NO ACTION' AND f.on_delete='NO ACTION' AND f.match='NONE') OR (m.name='sidecar_subtitles' AND f.id=0 AND f.seq=0 AND f."from"='file_id' AND f."to"='id' AND f.on_update='NO ACTION' AND f.on_delete='NO ACTION' AND f.match='NONE')))));
INSERT INTO motion_rebuild_assert VALUES((NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='motion_source_freeze_insert_media_files' AND sql<>'CREATE TRIGGER "motion_source_freeze_insert_media_files" BEFORE INSERT ON "media_files" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection(''media_files'')<>1 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END')));
INSERT INTO motion_rebuild_assert VALUES((NOT EXISTS(SELECT 1 FROM catabolic_source_guards WHERE trigger_name='motion_source_freeze_insert_media_files' AND (table_name<>'media_files' OR trigger_sql<>'CREATE TRIGGER "motion_source_freeze_insert_media_files" BEFORE INSERT ON "media_files" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection(''media_files'')<>1 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END'))));
INSERT INTO motion_rebuild_assert VALUES((NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='motion_source_freeze_update_media_files' AND sql<>'CREATE TRIGGER "motion_source_freeze_update_media_files" BEFORE UPDATE ON "media_files" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection(''media_files'')<>1 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END')));
INSERT INTO motion_rebuild_assert VALUES((NOT EXISTS(SELECT 1 FROM catabolic_source_guards WHERE trigger_name='motion_source_freeze_update_media_files' AND (table_name<>'media_files' OR trigger_sql<>'CREATE TRIGGER "motion_source_freeze_update_media_files" BEFORE UPDATE ON "media_files" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection(''media_files'')<>1 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END'))));
INSERT INTO motion_rebuild_assert VALUES((NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='motion_source_freeze_delete_media_files' AND sql<>'CREATE TRIGGER "motion_source_freeze_delete_media_files" BEFORE DELETE ON "media_files" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection(''media_files'')<>1 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END')));
INSERT INTO motion_rebuild_assert VALUES((NOT EXISTS(SELECT 1 FROM catabolic_source_guards WHERE trigger_name='motion_source_freeze_delete_media_files' AND (table_name<>'media_files' OR trigger_sql<>'CREATE TRIGGER "motion_source_freeze_delete_media_files" BEFORE DELETE ON "media_files" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection(''media_files'')<>1 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END'))));
INSERT INTO motion_rebuild_assert VALUES(((SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name='media_files' AND name LIKE 'motion_source_freeze_%') IN(0,3)));
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='trigger' AND tbl_name='media_files' AND name LIKE 'motion_source_freeze_%' AND name NOT IN('motion_source_freeze_insert_media_files','motion_source_freeze_update_media_files','motion_source_freeze_delete_media_files')));
INSERT INTO motion_rebuild_assert VALUES((SELECT count(*) FROM catabolic_source_guards WHERE table_name='media_files') IN(0,3) AND NOT EXISTS(SELECT 1 FROM catabolic_source_guards WHERE table_name='media_files' AND trigger_name NOT IN('motion_source_freeze_insert_media_files','motion_source_freeze_update_media_files','motion_source_freeze_delete_media_files')));
-- Never repair evidence that a previously installed source guard disappeared.
INSERT INTO motion_rebuild_assert VALUES((SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name='media_files' AND name LIKE 'motion_source_freeze_%')=(SELECT count(*) FROM catabolic_source_guards WHERE table_name='media_files'));
INSERT INTO motion_rebuild_assert VALUES((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1)=1 OR ((SELECT count(*) FROM catabolic_source_guards WHERE table_name='media_files')=3 AND (SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name='media_files' AND name LIKE 'motion_source_freeze_%')=3));
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='motion_source_freeze_insert__sqlx_migrations' AND sql<>'CREATE TRIGGER "motion_source_freeze_insert__sqlx_migrations" BEFORE INSERT ON "_sqlx_migrations" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END'));
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT 1 FROM catabolic_source_guards WHERE trigger_name='motion_source_freeze_insert__sqlx_migrations' AND (table_name<>'_sqlx_migrations' OR trigger_sql<>'CREATE TRIGGER "motion_source_freeze_insert__sqlx_migrations" BEFORE INSERT ON "_sqlx_migrations" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END')));
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='motion_source_freeze_update__sqlx_migrations' AND sql<>'CREATE TRIGGER "motion_source_freeze_update__sqlx_migrations" BEFORE UPDATE ON "_sqlx_migrations" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END'));
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT 1 FROM catabolic_source_guards WHERE trigger_name='motion_source_freeze_update__sqlx_migrations' AND (table_name<>'_sqlx_migrations' OR trigger_sql<>'CREATE TRIGGER "motion_source_freeze_update__sqlx_migrations" BEFORE UPDATE ON "_sqlx_migrations" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END')));
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='motion_source_freeze_delete__sqlx_migrations' AND sql<>'CREATE TRIGGER "motion_source_freeze_delete__sqlx_migrations" BEFORE DELETE ON "_sqlx_migrations" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END'));
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT 1 FROM catabolic_source_guards WHERE trigger_name='motion_source_freeze_delete__sqlx_migrations' AND (table_name<>'_sqlx_migrations' OR trigger_sql<>'CREATE TRIGGER "motion_source_freeze_delete__sqlx_migrations" BEFORE DELETE ON "_sqlx_migrations" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 BEGIN SELECT RAISE(ABORT,''Motion catalog source is frozen''); END')));
INSERT INTO motion_rebuild_assert VALUES((SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name='_sqlx_migrations')=(SELECT count(*) FROM catabolic_source_guards WHERE table_name='_sqlx_migrations') AND (SELECT count(*) FROM catabolic_source_guards WHERE table_name='_sqlx_migrations') IN(0,3));
INSERT INTO motion_rebuild_assert VALUES((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1)=1 OR (SELECT count(*) FROM catabolic_source_guards WHERE table_name='_sqlx_migrations')=3);
PRAGMA defer_foreign_keys=ON;
CREATE TEMP TABLE motion_media_backup AS SELECT * FROM media_files;
DROP VIEW catalog_files;
DROP TABLE media_files;
CREATE TABLE media_files (
 id TEXT PRIMARY KEY, edition_id TEXT NOT NULL REFERENCES editions(id), library_id TEXT NOT NULL REFERENCES libraries(id), relative_path TEXT NOT NULL,
 revision TEXT NOT NULL, fingerprint TEXT NOT NULL, bytes INTEGER NOT NULL,
 duration_seconds REAL, tracks_json TEXT NOT NULL DEFAULT '[]', available INTEGER NOT NULL DEFAULT 1, generated INTEGER NOT NULL DEFAULT 0, chapters_json TEXT,
 native_history INTEGER NOT NULL DEFAULT 0 CHECK(native_history IN(0,1)),
 CHECK(native_history=0 OR available=0)
);
INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds,tracks_json,available,generated,chapters_json) SELECT id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds,tracks_json,available,generated,chapters_json FROM motion_media_backup;
CREATE UNIQUE INDEX media_files_current_locator ON media_files(library_id,relative_path) WHERE native_history=0;
CREATE INDEX media_files_revision ON media_files(revision);
CREATE TRIGGER catalog_revision_file_change AFTER UPDATE OF edition_id,revision ON media_files
 WHEN OLD.edition_id IS NOT NEW.edition_id OR OLD.revision IS NOT NEW.revision BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (SELECT item_id FROM editions WHERE id IN (OLD.edition_id,NEW.edition_id)); END;
CREATE TRIGGER catalog_revision_file_delete AFTER DELETE ON media_files BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT item_id FROM editions WHERE id=OLD.edition_id); END;
CREATE TRIGGER catalog_revision_file_insert AFTER INSERT ON media_files BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT item_id FROM editions WHERE id=NEW.edition_id); END;
CREATE TRIGGER event_media_files_delete AFTER DELETE ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=OLD.library_id WHERE e.id=OLD.edition_id;
END;
CREATE TRIGGER event_media_files_insert AFTER INSERT ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=NEW.library_id WHERE e.id=NEW.edition_id;
END;
CREATE TRIGGER event_media_files_update AFTER UPDATE ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=OLD.library_id WHERE e.id=OLD.edition_id;
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',e.item_id,ls.library_id FROM editions e LEFT JOIN library_sources ls ON ls.source_id=NEW.library_id WHERE e.id=NEW.edition_id AND (NEW.edition_id!=OLD.edition_id OR NEW.library_id!=OLD.library_id);
END;
CREATE TRIGGER preparation_file_update AFTER UPDATE OF revision,available,library_id,edition_id ON media_files
WHEN OLD.revision IS NOT NEW.revision OR OLD.available IS NOT NEW.available OR OLD.library_id IS NOT NEW.library_id OR OLD.edition_id IS NOT NEW.edition_id BEGIN
 UPDATE preparations SET revision=revision+1 WHERE job_id IN (SELECT id FROM processing_jobs WHERE source_file_id=NEW.id OR output_file_id=NEW.id);
END;
CREATE VIEW catalog_files AS SELECT f.*,e.item_id,e.label AS edition_label,i.title,i.kind
 FROM media_files f JOIN editions e ON e.id=f.edition_id JOIN items i ON i.id=e.item_id;
CREATE TRIGGER "motion_source_freeze_insert_media_files" BEFORE INSERT ON "media_files" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection('media_files')<>1 BEGIN SELECT RAISE(ABORT,'Motion catalog source is frozen'); END;
CREATE TRIGGER "motion_source_freeze_update_media_files" BEFORE UPDATE ON "media_files" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection('media_files')<>1 BEGIN SELECT RAISE(ABORT,'Motion catalog source is frozen'); END;
CREATE TRIGGER "motion_source_freeze_delete_media_files" BEFORE DELETE ON "media_files" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection('media_files')<>1 BEGIN SELECT RAISE(ABORT,'Motion catalog source is frozen'); END;
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds,tracks_json,available,generated,chapters_json FROM media_files EXCEPT SELECT id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds,tracks_json,available,generated,chapters_json FROM motion_media_backup));
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds,tracks_json,available,generated,chapters_json FROM motion_media_backup EXCEPT SELECT id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds,tracks_json,available,generated,chapters_json FROM media_files));
INSERT INTO motion_rebuild_assert VALUES((SELECT count(*) FROM media_files)=(SELECT count(*) FROM motion_media_backup) AND NOT EXISTS(SELECT 1 FROM media_files WHERE native_history<>0));
INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check));
DROP TRIGGER IF EXISTS "motion_source_freeze_insert__sqlx_migrations";
CREATE TRIGGER "motion_source_freeze_insert__sqlx_migrations" BEFORE INSERT ON "_sqlx_migrations" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection('_sqlx_migrations')<>1 BEGIN SELECT RAISE(ABORT,'Motion catalog source is frozen'); END;
DROP TRIGGER IF EXISTS "motion_source_freeze_update__sqlx_migrations";
CREATE TRIGGER "motion_source_freeze_update__sqlx_migrations" BEFORE UPDATE ON "_sqlx_migrations" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection('_sqlx_migrations')<>1 BEGIN SELECT RAISE(ABORT,'Motion catalog source is frozen'); END;
DROP TRIGGER IF EXISTS "motion_source_freeze_delete__sqlx_migrations";
CREATE TRIGGER "motion_source_freeze_delete__sqlx_migrations" BEFORE DELETE ON "_sqlx_migrations" WHEN coalesce((SELECT writes_allowed FROM catabolic_source_state WHERE singleton=1),0)=0 AND motion_native_projection('_sqlx_migrations')<>1 BEGIN SELECT RAISE(ABORT,'Motion catalog source is frozen'); END;
DROP TABLE motion_media_backup;
DROP TABLE motion_rebuild_assert;
PRAGMA defer_foreign_keys=OFF;
