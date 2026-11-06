-- Whole-resource validators must survive deletion and recreation of sources.
-- Retain rows after item removal so an identifier cannot acquire an old token.
CREATE TABLE metadata_resource_generations (
 item_id TEXT PRIMARY KEY,
 generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation > 0)
);
INSERT INTO metadata_resource_generations(item_id,generation)
 SELECT id,1 FROM items;

CREATE TRIGGER metadata_generation_document_insert AFTER INSERT ON metadata_documents BEGIN
 INSERT INTO metadata_resource_generations VALUES (NEW.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
END;
CREATE TRIGGER metadata_generation_document_delete AFTER DELETE ON metadata_documents BEGIN
 INSERT INTO metadata_resource_generations VALUES (OLD.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
END;
CREATE TRIGGER metadata_generation_document_update_same AFTER UPDATE ON metadata_documents
WHEN OLD.item_id=NEW.item_id AND (OLD.source IS NOT NEW.source OR OLD.revision IS NOT NEW.revision OR OLD.external_id IS NOT NEW.external_id OR OLD.document_json IS NOT NEW.document_json OR OLD.updated_at IS NOT NEW.updated_at) BEGIN
 INSERT INTO metadata_resource_generations VALUES (NEW.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
END;
CREATE TRIGGER metadata_generation_document_update_move AFTER UPDATE OF item_id ON metadata_documents
WHEN OLD.item_id<>NEW.item_id BEGIN
 INSERT INTO metadata_resource_generations VALUES (OLD.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
 INSERT INTO metadata_resource_generations VALUES (NEW.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
END;
CREATE TRIGGER metadata_generation_origin_insert AFTER INSERT ON item_origins BEGIN
 INSERT INTO metadata_resource_generations VALUES (NEW.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
END;
CREATE TRIGGER metadata_generation_origin_delete AFTER DELETE ON item_origins BEGIN
 INSERT INTO metadata_resource_generations VALUES (OLD.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
END;
CREATE TRIGGER metadata_generation_origin_update_same AFTER UPDATE ON item_origins
WHEN OLD.item_id=NEW.item_id AND OLD.title IS NOT NEW.title BEGIN
 INSERT INTO metadata_resource_generations VALUES (NEW.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
END;
CREATE TRIGGER metadata_generation_origin_update_move AFTER UPDATE OF item_id ON item_origins
WHEN OLD.item_id<>NEW.item_id BEGIN
 INSERT INTO metadata_resource_generations VALUES (OLD.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
 INSERT INTO metadata_resource_generations VALUES (NEW.item_id,1)
 ON CONFLICT(item_id) DO UPDATE SET generation=generation+1;
END;
