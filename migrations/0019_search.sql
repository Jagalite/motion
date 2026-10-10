-- Rebuildable search projection over live works. Canonical rows mark works
-- dirty; the adapter rebuilds their documents (core::search::body) in batches.
-- Tokens are already normalized words, so the tokenizer keeps diacritics.
-- Triggers avoid INSERT OR IGNORE: an UPSERT that fires them would override
-- the trigger's conflict handling and turn a duplicate into an error.
CREATE VIRTUAL TABLE search_fts USING fts5(item_id UNINDEXED, body, tokenize='unicode61 remove_diacritics 0');
CREATE TABLE search_dirty (item_id TEXT PRIMARY KEY);
INSERT INTO search_dirty SELECT id FROM items;
CREATE TRIGGER search_items_insert AFTER INSERT ON items BEGIN INSERT INTO search_dirty SELECT NEW.id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=NEW.id); END;
CREATE TRIGGER search_items_title AFTER UPDATE OF title ON items BEGIN INSERT INTO search_dirty SELECT NEW.id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=NEW.id); END;
CREATE TRIGGER search_metadata_insert AFTER INSERT ON metadata_documents BEGIN INSERT INTO search_dirty SELECT NEW.item_id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=NEW.item_id); END;
CREATE TRIGGER search_metadata_update AFTER UPDATE ON metadata_documents BEGIN
 INSERT INTO search_dirty SELECT NEW.item_id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=NEW.item_id); INSERT INTO search_dirty SELECT OLD.item_id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=OLD.item_id); END;
CREATE TRIGGER search_metadata_delete AFTER DELETE ON metadata_documents BEGIN INSERT INTO search_dirty SELECT OLD.item_id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=OLD.item_id); END;
CREATE TRIGGER search_alias_insert AFTER INSERT ON item_aliases BEGIN
 INSERT INTO search_dirty SELECT NEW.item_id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=NEW.item_id); INSERT INTO search_dirty SELECT NEW.alias_id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=NEW.alias_id); END;
CREATE TRIGGER search_alias_update AFTER UPDATE ON item_aliases BEGIN
 INSERT INTO search_dirty SELECT NEW.item_id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=NEW.item_id); INSERT INTO search_dirty SELECT OLD.item_id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=OLD.item_id); END;
