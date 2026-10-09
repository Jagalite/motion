-- Timelines, media versions and version-to-file bindings become real rows.
-- media_files.edition_id stays the authority for a file's edition; versions
-- own which content represents which timeline. A binding pins the reviewed
-- content revision; availability is derived from occurrences, never stored.

CREATE TABLE timelines (
 id TEXT PRIMARY KEY,
 edition_id TEXT NOT NULL REFERENCES editions(id),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision >= 1),
 duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms >= 0),
 order_group_id TEXT,
 order_position INTEGER CHECK(order_position IS NULL OR order_position >= 0)
);
CREATE INDEX timelines_edition ON timelines(edition_id,id);
-- Every existing edition keeps one timeline whose ID is the edition ID, so
-- legacy progress attribution, playlists and queues remain valid.
INSERT INTO timelines (id,edition_id) SELECT id,id FROM editions;
-- New editions get timelines from explicit catalog operations
-- (curation::create_edition creates the default one).

CREATE TABLE media_versions (
 id TEXT PRIMARY KEY,
 timeline_id TEXT NOT NULL REFERENCES timelines(id),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision >= 1),
 label TEXT NOT NULL DEFAULT '',
 origin TEXT NOT NULL CHECK(origin IN ('original','generated')),
 equivalence TEXT NOT NULL CHECK(equivalence IN ('verified','declared','unknown'))
);
CREATE INDEX media_versions_timeline ON media_versions(timeline_id,id);
CREATE TABLE version_files (
 version_id TEXT NOT NULL REFERENCES media_versions(id) ON DELETE CASCADE,
 part INTEGER NOT NULL CHECK(part >= 1),
 file_id TEXT NOT NULL REFERENCES media_files(id),
 file_revision TEXT NOT NULL,
 start_ms INTEGER CHECK(start_ms IS NULL OR start_ms >= 0),
 end_ms INTEGER CHECK(end_ms IS NULL OR end_ms >= 0),
 CHECK (start_ms IS NULL OR end_ms IS NULL OR start_ms < end_ms),
 PRIMARY KEY(version_id,part),
 UNIQUE(version_id,file_id)
);
CREATE INDEX version_files_file ON version_files(file_id);

-- Backfill: one version per (edition, content revision), represented by its
-- smallest file ID; other files with that content are copies (occurrences).
-- Legacy assignments were operator decisions, hence 'declared'.
INSERT INTO media_versions (id,timeline_id,origin,equivalence)
 SELECT min(id),edition_id,CASE WHEN max(generated)=1 THEN 'generated' ELSE 'original' END,'declared'
 FROM media_files GROUP BY edition_id,revision;
INSERT INTO version_files (version_id,part,file_id,file_revision)
 SELECT v.id,1,f.id,f.revision FROM media_versions v JOIN media_files f ON f.id=v.id;

-- A binding's file must belong to the edition that owns the version's timeline.
CREATE TRIGGER version_files_same_edition BEFORE INSERT ON version_files
 WHEN (SELECT edition_id FROM media_files WHERE id=NEW.file_id) IS NOT
      (SELECT t.edition_id FROM media_versions v JOIN timelines t ON t.id=v.timeline_id WHERE v.id=NEW.version_id) BEGIN
 SELECT RAISE(ABORT,'bound file belongs to another edition'); END;
-- Diagnostic: must be empty after every committed catalog transaction.
CREATE VIEW version_edition_mismatch AS
 SELECT b.version_id,b.file_id FROM version_files b
 JOIN media_versions v ON v.id=b.version_id JOIN timelines t ON t.id=v.timeline_id
 JOIN media_files f ON f.id=b.file_id WHERE f.edition_id IS NOT t.edition_id;

-- Structural revision of the owning work.
CREATE TRIGGER catalog_revision_timeline_change AFTER UPDATE ON timelines BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (SELECT item_id FROM editions WHERE id IN (OLD.edition_id,NEW.edition_id)); END;
CREATE TRIGGER catalog_revision_timeline_insert AFTER INSERT ON timelines BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT item_id FROM editions WHERE id=NEW.edition_id); END;
CREATE TRIGGER catalog_revision_version_insert AFTER INSERT ON media_versions BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=NEW.timeline_id); END;
CREATE TRIGGER catalog_revision_version_change AFTER UPDATE ON media_versions BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (SELECT e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id IN (OLD.timeline_id,NEW.timeline_id)); END;
CREATE TRIGGER catalog_revision_version_delete AFTER DELETE ON media_versions BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=OLD.timeline_id); END;
CREATE TRIGGER catalog_revision_binding_insert AFTER INSERT ON version_files BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT e.item_id FROM media_versions v JOIN timelines t ON t.id=v.timeline_id JOIN editions e ON e.id=t.edition_id WHERE v.id=NEW.version_id); END;
CREATE TRIGGER catalog_revision_binding_change AFTER UPDATE ON version_files BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT e.item_id FROM media_versions v JOIN timelines t ON t.id=v.timeline_id JOIN editions e ON e.id=t.edition_id WHERE v.id=NEW.version_id); END;

-- Timeline references now point at real timelines (leaf tables, rebuilt).
CREATE TABLE playlist_entries_new (
 playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position >= 0),
 entry_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL REFERENCES timelines(id),
 PRIMARY KEY(playlist_id,position),
 UNIQUE(playlist_id,entry_id)
);
INSERT INTO playlist_entries_new SELECT * FROM playlist_entries;
DROP TABLE playlist_entries;
ALTER TABLE playlist_entries_new RENAME TO playlist_entries;
CREATE TABLE queue_entries_new (
 queue_id TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position >= 0),
 entry_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL REFERENCES timelines(id),
 PRIMARY KEY(queue_id,position),
 UNIQUE(queue_id,entry_id)
);
INSERT INTO queue_entries_new SELECT * FROM queue_entries;
DROP TABLE queue_entries;
ALTER TABLE queue_entries_new RENAME TO queue_entries;
CREATE TABLE legacy_progress_attribution_new (
 profile_id TEXT NOT NULL REFERENCES profiles(id),
 item_id TEXT NOT NULL REFERENCES items(id),
 outcome TEXT NOT NULL CHECK(outcome IN ('exact','ambiguous','orphaned')),
 timeline_id TEXT REFERENCES timelines(id),
 candidates_json TEXT NOT NULL,
 receipt_id TEXT NOT NULL REFERENCES catalog_receipts(id),
 PRIMARY KEY(profile_id,item_id),
 CHECK ((outcome='exact') = (timeline_id IS NOT NULL))
);
INSERT INTO legacy_progress_attribution_new SELECT * FROM legacy_progress_attribution;
DROP TABLE legacy_progress_attribution;
ALTER TABLE legacy_progress_attribution_new RENAME TO legacy_progress_attribution;
