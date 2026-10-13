-- Role readback includes provider/local contributions as well as explicit pins.
-- Its item ETag must change whenever those facts change, including ownership moves.
CREATE TRIGGER catalog_revision_artwork_contribution_insert
AFTER INSERT ON artwork_contributions BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=NEW.item_id;
END;
CREATE TRIGGER catalog_revision_artwork_contribution_update
AFTER UPDATE ON artwork_contributions BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id IN (OLD.item_id,NEW.item_id);
END;
CREATE TRIGGER catalog_revision_artwork_contribution_delete
AFTER DELETE ON artwork_contributions BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1 WHERE id=OLD.item_id;
END;
