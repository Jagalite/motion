-- A02: representation revisions and unselected uploaded artwork ownership.
CREATE TRIGGER timeline_version_insert AFTER INSERT ON media_versions BEGIN
 UPDATE timelines SET revision=revision+1 WHERE id=NEW.timeline_id; END;
CREATE TRIGGER timeline_version_delete AFTER DELETE ON media_versions BEGIN
 UPDATE timelines SET revision=revision+1 WHERE id=OLD.timeline_id; END;
CREATE TRIGGER timeline_version_update AFTER UPDATE ON media_versions BEGIN
 UPDATE timelines SET revision=revision+1 WHERE id IN (OLD.timeline_id,NEW.timeline_id); END;
CREATE TABLE item_artwork_assets (
 item_id TEXT NOT NULL REFERENCES items(id),
 asset_id TEXT NOT NULL REFERENCES artwork_assets(id),
 PRIMARY KEY(item_id,asset_id)
);

-- Preserve selections while allowing the contract's banner role.
CREATE TABLE artwork_selections_v2 (
 item_id TEXT NOT NULL REFERENCES items(id), role TEXT NOT NULL CHECK(role IN ('poster','backdrop','thumbnail','banner')),
 asset_id TEXT REFERENCES artwork_assets(id), revision INTEGER NOT NULL,
 PRIMARY KEY(item_id,role)
);
INSERT INTO artwork_selections_v2 SELECT * FROM artwork_selections;
DROP TABLE artwork_selections;
ALTER TABLE artwork_selections_v2 RENAME TO artwork_selections;
CREATE TRIGGER event_artwork_selections_insert AFTER INSERT ON artwork_selections BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('artwork',NEW.item_id); UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=NEW.item_id; END;
CREATE TRIGGER event_artwork_selections_update AFTER UPDATE ON artwork_selections BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('artwork',NEW.item_id); UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=NEW.item_id; END;
CREATE TRIGGER event_artwork_selections_delete AFTER DELETE ON artwork_selections BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('artwork',OLD.item_id); UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=OLD.item_id; END;
