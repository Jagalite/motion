-- Catalog identity foundations. Legacy editions are single-timeline: a legacy
-- timeline's ID is its edition's ID, and a legacy version is the set of one
-- edition's files sharing a content revision (copies are extra occurrences).

-- Immutable audit/migration receipts for merges, splits and data upgrades.
CREATE TABLE catalog_receipts (
 id TEXT PRIMARY KEY, kind TEXT NOT NULL, created_at INTEGER NOT NULL, document_json TEXT NOT NULL
);
CREATE TRIGGER catalog_receipts_no_update BEFORE UPDATE ON catalog_receipts BEGIN SELECT RAISE(ABORT,'catalog receipts are immutable'); END;
CREATE TRIGGER catalog_receipts_no_delete BEFORE DELETE ON catalog_receipts BEGIN SELECT RAISE(ABORT,'catalog receipts are immutable'); END;

-- Structural revision of a logical work. Merge/split plans fence on it, so every
-- writer that changes editions, file associations, external identities or
-- hierarchy must invalidate outstanding plans. Triggers make that unconditional.
ALTER TABLE items ADD COLUMN catalog_revision INTEGER NOT NULL DEFAULT 1;
CREATE TRIGGER catalog_revision_edition_insert AFTER INSERT ON editions BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=NEW.item_id; END;
CREATE TRIGGER catalog_revision_edition_delete AFTER DELETE ON editions BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=OLD.item_id; END;
CREATE TRIGGER catalog_revision_edition_move AFTER UPDATE OF item_id ON editions WHEN OLD.item_id IS NOT NEW.item_id BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (OLD.item_id,NEW.item_id); END;
CREATE TRIGGER catalog_revision_file_insert AFTER INSERT ON media_files BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT item_id FROM editions WHERE id=NEW.edition_id); END;
CREATE TRIGGER catalog_revision_file_delete AFTER DELETE ON media_files BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=(SELECT item_id FROM editions WHERE id=OLD.edition_id); END;
CREATE TRIGGER catalog_revision_file_change AFTER UPDATE OF edition_id,revision ON media_files
 WHEN OLD.edition_id IS NOT NEW.edition_id OR OLD.revision IS NOT NEW.revision BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (SELECT item_id FROM editions WHERE id IN (OLD.edition_id,NEW.edition_id)); END;
CREATE TRIGGER catalog_revision_identity_insert AFTER INSERT ON metadata_documents BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=NEW.item_id; END;
CREATE TRIGGER catalog_revision_identity_delete AFTER DELETE ON metadata_documents BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=OLD.item_id; END;
CREATE TRIGGER catalog_revision_identity_change AFTER UPDATE OF item_id,source,external_id ON metadata_documents
 WHEN OLD.item_id IS NOT NEW.item_id OR OLD.source IS NOT NEW.source OR OLD.external_id IS NOT NEW.external_id BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (OLD.item_id,NEW.item_id); END;
CREATE TRIGGER catalog_revision_structure_insert AFTER INSERT ON item_structure BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (NEW.item_id,NEW.parent_id); END;
CREATE TRIGGER catalog_revision_structure_update AFTER UPDATE ON item_structure BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (NEW.item_id,OLD.parent_id,NEW.parent_id); END;
CREATE TRIGGER catalog_revision_structure_delete AFTER DELETE ON item_structure BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (OLD.item_id,OLD.parent_id); END;

-- Retired works keep their rows (history, sessions and contributions still
-- reference them) and resolve to one live work.
CREATE TABLE item_aliases (
 alias_id TEXT PRIMARY KEY REFERENCES items(id),
 item_id TEXT NOT NULL REFERENCES items(id),
 receipt_id TEXT NOT NULL REFERENCES catalog_receipts(id),
 CHECK (alias_id <> item_id)
);
CREATE INDEX item_aliases_target ON item_aliases(item_id);
CREATE TRIGGER item_aliases_target_live BEFORE INSERT ON item_aliases
 WHEN EXISTS (SELECT 1 FROM item_aliases WHERE alias_id=NEW.item_id) BEGIN
 SELECT RAISE(ABORT,'alias target is retired'); END;
CREATE TRIGGER item_aliases_repoint_live BEFORE UPDATE OF item_id ON item_aliases
 WHEN EXISTS (SELECT 1 FROM item_aliases WHERE alias_id=NEW.item_id) BEGIN
 SELECT RAISE(ABORT,'alias target is retired'); END;

-- Source binding revision changes only on a validated rebind (relocation).
ALTER TABLE libraries ADD COLUMN binding_revision INTEGER NOT NULL DEFAULT 1;

-- Work-keyed legacy progress attributed to a timeline only when unambiguous.
-- The original progress/viewing rows are retained unchanged.
CREATE TABLE legacy_progress_attribution (
 profile_id TEXT NOT NULL REFERENCES profiles(id),
 item_id TEXT NOT NULL REFERENCES items(id),
 outcome TEXT NOT NULL CHECK(outcome IN ('exact','ambiguous','orphaned')),
 timeline_id TEXT REFERENCES editions(id),
 candidates_json TEXT NOT NULL,
 receipt_id TEXT NOT NULL REFERENCES catalog_receipts(id),
 PRIMARY KEY(profile_id,item_id),
 CHECK ((outcome='exact') = (timeline_id IS NOT NULL))
);
-- Aliases stay one step deep: re-point aliases of a work before retiring it.
CREATE TRIGGER item_aliases_source_unreferenced BEFORE INSERT ON item_aliases
 WHEN EXISTS (SELECT 1 FROM item_aliases WHERE item_id=NEW.alias_id) BEGIN
 SELECT RAISE(ABORT,'re-point aliases before retiring their target'); END;
