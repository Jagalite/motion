-- Profile-owned organization. Membership and ordering never grant access.
-- Timeline IDs are legacy edition IDs until timelines become their own table.
CREATE TABLE saved_filters (
 id TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision >= 1),
 profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
 name TEXT NOT NULL, terms_json TEXT NOT NULL
);
CREATE INDEX saved_filters_profile ON saved_filters(profile_id,name,id);
CREATE TABLE collections (
 id TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision >= 1),
 profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
 name TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('manual','smart')),
 filter_id TEXT REFERENCES saved_filters(id),
 CHECK ((kind='manual') = (filter_id IS NULL))
);
CREATE INDEX collections_profile ON collections(profile_id,name,id);
CREATE TABLE collection_items (
 collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
 item_id TEXT NOT NULL REFERENCES items(id),
 PRIMARY KEY(collection_id,item_id)
);
CREATE TABLE playlists (
 id TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision >= 1),
 profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
 name TEXT NOT NULL
);
CREATE INDEX playlists_profile ON playlists(profile_id,name,id);
CREATE TABLE playlist_entries (
 playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position >= 0),
 entry_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL REFERENCES editions(id),
 PRIMARY KEY(playlist_id,position),
 UNIQUE(playlist_id,entry_id)
);
-- Queues are per profile, never a global mutable "current queue".
CREATE TABLE queues (
 id TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision >= 1),
 profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
 repeat TEXT NOT NULL CHECK(repeat IN ('off','one','all')),
 shuffle_seed TEXT CHECK(shuffle_seed IS NULL OR (shuffle_seed GLOB '[0-9]*' AND length(shuffle_seed) <= 20)),
 current_entry_id TEXT
);
CREATE INDEX queues_profile ON queues(profile_id,id);
CREATE TABLE queue_entries (
 queue_id TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position >= 0),
 entry_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL REFERENCES editions(id),
 PRIMARY KEY(queue_id,position),
 UNIQUE(queue_id,entry_id)
);
CREATE TRIGGER event_collections_change AFTER UPDATE ON collections BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('organization',NEW.id); END;
CREATE TRIGGER event_collections_insert AFTER INSERT ON collections BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('organization',NEW.id); END;
CREATE TRIGGER event_collections_delete AFTER DELETE ON collections BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('organization',OLD.id); END;
CREATE TRIGGER event_saved_filters_insert AFTER INSERT ON saved_filters BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('organization',NEW.id); END;
CREATE TRIGGER event_saved_filters_change AFTER UPDATE ON saved_filters BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('organization',NEW.id); END;
CREATE TRIGGER event_playlists_insert AFTER INSERT ON playlists BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('organization',NEW.id); END;
CREATE TRIGGER event_playlists_change AFTER UPDATE ON playlists BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('organization',NEW.id); END;
CREATE TRIGGER event_queues_insert AFTER INSERT ON queues BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('queues',NEW.profile_id); END;
CREATE TRIGGER event_queues_change AFTER UPDATE ON queues BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('queues',NEW.profile_id); END;
