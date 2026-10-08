CREATE TABLE libraries (id TEXT PRIMARY KEY, name TEXT NOT NULL, root TEXT NOT NULL UNIQUE, root_identity TEXT NOT NULL);
CREATE TABLE jobs (id TEXT PRIMARY KEY, library_id TEXT NOT NULL REFERENCES libraries(id), phase TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0, error TEXT, created_at INTEGER NOT NULL);
CREATE UNIQUE INDEX one_active_scan ON jobs(library_id) WHERE phase IN ('queued','running','cancelling');
CREATE TABLE items (id TEXT PRIMARY KEY, title TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('video','audio')));
CREATE TABLE editions (id TEXT PRIMARY KEY, item_id TEXT NOT NULL REFERENCES items(id), label TEXT NOT NULL);
CREATE TABLE media_files (
 id TEXT PRIMARY KEY, edition_id TEXT NOT NULL REFERENCES editions(id), library_id TEXT NOT NULL REFERENCES libraries(id), relative_path TEXT NOT NULL,
 revision TEXT NOT NULL, fingerprint TEXT NOT NULL, bytes INTEGER NOT NULL,
 duration_seconds REAL, tracks_json TEXT NOT NULL DEFAULT '[]', available INTEGER NOT NULL DEFAULT 1,
 UNIQUE(library_id, relative_path)
);
CREATE INDEX items_browse ON items(title, id);
CREATE VIEW catalog_files AS SELECT f.*,e.item_id,e.label AS edition_label,i.title,i.kind
 FROM media_files f JOIN editions e ON e.id=f.edition_id JOIN items i ON i.id=e.item_id;
CREATE TABLE profiles (id TEXT PRIMARY KEY, name TEXT NOT NULL);
INSERT INTO profiles VALUES ('default', 'Everyone');
CREATE TABLE progress (profile_id TEXT NOT NULL REFERENCES profiles(id), item_id TEXT NOT NULL REFERENCES items(id),
 position_seconds REAL NOT NULL CHECK(position_seconds >= 0), updated_at INTEGER NOT NULL,
 PRIMARY KEY(profile_id, item_id));
