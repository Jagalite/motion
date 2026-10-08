CREATE TABLE item_structure (
 item_id TEXT PRIMARY KEY REFERENCES items(id),
 media_type TEXT NOT NULL CHECK(media_type IN ('unclassified','movie','series','season','episode')),
 parent_id TEXT REFERENCES items(id), number INTEGER, revision INTEGER NOT NULL DEFAULT 1,
 CHECK ((media_type IN ('season','episode') AND parent_id IS NOT NULL AND number >= 0) OR
        (media_type IN ('unclassified','movie','series') AND parent_id IS NULL AND number IS NULL))
);
CREATE UNIQUE INDEX numbered_children ON item_structure(parent_id,number) WHERE parent_id IS NOT NULL;
CREATE INDEX structure_parent ON item_structure(parent_id,item_id);
ALTER TABLE editions ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
CREATE TABLE artwork_assets (id TEXT PRIMARY KEY, mime TEXT NOT NULL, width INTEGER NOT NULL, height INTEGER NOT NULL, bytes BLOB NOT NULL);
CREATE TABLE artwork_contributions (
 item_id TEXT NOT NULL REFERENCES items(id), role TEXT NOT NULL CHECK(role IN ('poster','backdrop','thumbnail')),
 source TEXT NOT NULL, asset_id TEXT NOT NULL REFERENCES artwork_assets(id), revision INTEGER NOT NULL,
 PRIMARY KEY(item_id,role,source)
);
CREATE TABLE artwork_selections (
 item_id TEXT NOT NULL REFERENCES items(id), role TEXT NOT NULL CHECK(role IN ('poster','backdrop','thumbnail')),
 asset_id TEXT REFERENCES artwork_assets(id), revision INTEGER NOT NULL,
 PRIMARY KEY(item_id,role)
);
