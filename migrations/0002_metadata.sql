CREATE TABLE metadata_documents (
 item_id TEXT NOT NULL REFERENCES items(id), source TEXT NOT NULL, revision INTEGER NOT NULL,
 external_id TEXT, document_json TEXT NOT NULL, updated_at INTEGER NOT NULL,
 PRIMARY KEY(item_id,source)
);
CREATE UNIQUE INDEX external_item_identity ON metadata_documents(source,external_id) WHERE external_id IS NOT NULL;
CREATE TABLE item_origins (item_id TEXT PRIMARY KEY REFERENCES items(id), title TEXT NOT NULL);
INSERT INTO item_origins SELECT id,title FROM items;
