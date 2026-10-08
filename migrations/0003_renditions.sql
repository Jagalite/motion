CREATE TABLE renditions (
 id TEXT PRIMARY KEY, item_id TEXT NOT NULL REFERENCES items(id), source TEXT NOT NULL, external_id TEXT NOT NULL,
 file_id TEXT NOT NULL REFERENCES media_files(id), file_revision TEXT NOT NULL,
 source_file_id TEXT NOT NULL REFERENCES media_files(id), source_revision TEXT NOT NULL,
 label TEXT NOT NULL, recipe_json TEXT NOT NULL, updated_at INTEGER NOT NULL,
 UNIQUE(source,external_id)
);
