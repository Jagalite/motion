-- Explicit catalog structure (A01): episode order groups, item relationships,
-- and multi-episode bindings.

-- An order group belongs to a series release (edition) and orders the
-- timelines of its episodes (aired, DVD, absolute or custom order).
CREATE TABLE order_groups (
 id TEXT PRIMARY KEY,
 edition_id TEXT NOT NULL REFERENCES editions(id),
 name TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('aired','dvd','absolute','custom')),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision >= 1)
);
CREATE UNIQUE INDEX timelines_order_position ON timelines(order_group_id,order_position)
 WHERE order_group_id IS NOT NULL;

CREATE TABLE item_relationships (
 id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision >= 1),
 source_item_id TEXT NOT NULL REFERENCES items(id),
 target_item_id TEXT NOT NULL REFERENCES items(id),
 kind TEXT NOT NULL CHECK(kind IN ('part_of','edition_of','performed_by','created_by','extra_of','derived_from')),
 position INTEGER CHECK(position IS NULL OR position >= 0),
 UNIQUE(source_item_id,target_item_id,kind),
 CHECK (source_item_id <> target_item_id)
);
CREATE INDEX item_relationships_target ON item_relationships(target_item_id,kind);
CREATE TRIGGER catalog_revision_relationship_insert AFTER INSERT ON item_relationships BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (NEW.source_item_id,NEW.target_item_id); END;
CREATE TRIGGER catalog_revision_relationship_delete AFTER DELETE ON item_relationships BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (OLD.source_item_id,OLD.target_item_id); END;

-- A part may bind a file of another edition only with a known interval
-- (identity::binding_edition_allowed): a multi-episode file shared by several
-- episodes' timelines.
DROP TRIGGER version_files_same_edition;
CREATE TRIGGER version_files_same_edition BEFORE INSERT ON version_files
 WHEN (NEW.start_ms IS NULL OR NEW.end_ms IS NULL) AND
      (SELECT edition_id FROM media_files WHERE id=NEW.file_id) IS NOT
      (SELECT t.edition_id FROM media_versions v JOIN timelines t ON t.id=v.timeline_id WHERE v.id=NEW.version_id) BEGIN
 SELECT RAISE(ABORT,'bound file belongs to another edition'); END;
DROP VIEW version_edition_mismatch;
CREATE VIEW version_edition_mismatch AS
 SELECT b.version_id,b.file_id FROM version_files b
 JOIN media_versions v ON v.id=b.version_id JOIN timelines t ON t.id=v.timeline_id
 JOIN media_files f ON f.id=b.file_id
 WHERE f.edition_id IS NOT t.edition_id AND (b.start_ms IS NULL OR b.end_ms IS NULL);
