CREATE TABLE playback_sessions (
 id TEXT PRIMARY KEY, profile_id TEXT NOT NULL REFERENCES profiles(id), item_id TEXT NOT NULL REFERENCES items(id),
 file_id TEXT NOT NULL REFERENCES media_files(id), file_revision TEXT NOT NULL, duration_seconds REAL,
 sequence INTEGER NOT NULL DEFAULT 0, position_seconds REAL NOT NULL, status TEXT NOT NULL,
 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE INDEX sessions_profile_item ON playback_sessions(profile_id,item_id);
CREATE TABLE viewing_state (
 profile_id TEXT NOT NULL REFERENCES profiles(id), item_id TEXT NOT NULL REFERENCES items(id),
 automatic_watched INTEGER NOT NULL DEFAULT 0, manual_watched INTEGER,
 revision INTEGER NOT NULL DEFAULT 0, session_id TEXT REFERENCES playback_sessions(id),
 PRIMARY KEY(profile_id,item_id)
);
INSERT INTO viewing_state(profile_id,item_id) SELECT profile_id,item_id FROM progress;
CREATE TABLE playback_preferences (
 profile_id TEXT PRIMARY KEY REFERENCES profiles(id), revision INTEGER NOT NULL, document_json TEXT NOT NULL
);
