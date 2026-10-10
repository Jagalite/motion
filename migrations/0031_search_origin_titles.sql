-- Origin titles became editable (v2 item replacement) and are part of search
-- documents: the work itself and, if retired, its surviving work.
CREATE TRIGGER search_origin_title AFTER UPDATE OF title ON item_origins BEGIN
 INSERT INTO search_dirty SELECT NEW.item_id WHERE NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=NEW.item_id);
 INSERT INTO search_dirty SELECT a.item_id FROM item_aliases a WHERE a.alias_id=NEW.item_id AND NOT EXISTS (SELECT 1 FROM search_dirty WHERE item_id=a.item_id);
END;
