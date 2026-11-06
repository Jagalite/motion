-- A replacement must never overwrite bytes still named by a committed row.
-- NULL retains the existing file/revision directory for upgraded databases.
ALTER TABLE previews ADD COLUMN storage_key TEXT;
CREATE UNIQUE INDEX preview_storage_key ON previews(storage_key) WHERE storage_key IS NOT NULL;
-- Pending removal is durable independently of the catalog/job lifetime.
CREATE TABLE preview_removals (directory TEXT PRIMARY KEY NOT NULL);
CREATE TRIGGER preview_withdrawal_removal AFTER DELETE ON previews BEGIN
 INSERT OR IGNORE INTO preview_removals(directory)
 VALUES (CASE WHEN OLD.storage_key IS NULL THEN OLD.file_id || '/' || OLD.file_revision
              ELSE '.published/' || OLD.storage_key END);
END;
CREATE TRIGGER preview_replacement_removal AFTER UPDATE OF storage_key ON previews
WHEN OLD.storage_key IS NOT NEW.storage_key BEGIN
 INSERT OR IGNORE INTO preview_removals(directory)
 VALUES (CASE WHEN OLD.storage_key IS NULL THEN OLD.file_id || '/' || OLD.file_revision
              ELSE '.published/' || OLD.storage_key END);
END;
-- Persist progress even when an individual removal remains blocked forever.
CREATE TABLE preview_removal_cursor (id INTEGER PRIMARY KEY CHECK(id=1), after_directory TEXT NOT NULL);
INSERT INTO preview_removal_cursor VALUES (1,'');
