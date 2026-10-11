-- Smart representations depend on catalog facts, scope and canonical viewing.
-- Conservative invalidation is durable and shares the mutation transaction.
-- Organization events are excluded to prevent revision/event recursion.
CREATE TRIGGER smart_collection_fact_revision AFTER INSERT ON change_events
WHEN NEW.topic IN ('catalog','metadata','libraries','sources','devices','profiles')
BEGIN UPDATE collections SET revision=revision+1 WHERE kind='smart'; END;
CREATE TRIGGER smart_collection_viewing_revision AFTER INSERT ON change_events
WHEN NEW.topic='viewing'
BEGIN UPDATE collections SET revision=revision+1 WHERE kind='smart' AND profile_id=NEW.resource_id; END;
CREATE TRIGGER smart_collection_filter_revision AFTER UPDATE ON saved_filters
BEGIN UPDATE collections SET revision=revision+1 WHERE kind='smart' AND filter_id=NEW.id; END;
-- Binding changes can change which cut's watched facts a scoped reader sees.
CREATE TRIGGER smart_collection_binding_insert AFTER INSERT ON version_files
BEGIN UPDATE collections SET revision=revision+1 WHERE kind='smart'; END;
CREATE TRIGGER smart_collection_binding_update AFTER UPDATE ON version_files
BEGIN UPDATE collections SET revision=revision+1 WHERE kind='smart'; END;
CREATE TRIGGER smart_collection_binding_delete AFTER DELETE ON version_files
BEGIN UPDATE collections SET revision=revision+1 WHERE kind='smart'; END;
