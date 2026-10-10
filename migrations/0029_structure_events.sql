-- Review follow-ups for 0014/0023.
-- Remapped relationships (merges) change both endpoints' structure.
CREATE TRIGGER catalog_revision_relationship_update AFTER UPDATE ON item_relationships BEGIN
 UPDATE items SET catalog_revision=catalog_revision+1
  WHERE id IN (OLD.source_item_id,OLD.target_item_id,NEW.source_item_id,NEW.target_item_id); END;
-- Deleting organization resources emits invalidation hints like other changes.
CREATE TRIGGER event_saved_filters_delete AFTER DELETE ON saved_filters BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('organization',OLD.id); END;
CREATE TRIGGER event_playlists_delete AFTER DELETE ON playlists BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('organization',OLD.id); END;
CREATE TRIGGER event_queues_delete AFTER DELETE ON queues BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('queues',OLD.profile_id); END;
