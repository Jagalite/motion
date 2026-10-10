-- A08 identity and access. Number 0011 leaves 0010 to the concurrent A02
-- catalog-identity migration; A02 owns final sequencing.
-- Credential secrets are HMAC-derived from a key file outside the database and
-- a stored nonce; rows keep only SHA-256 hashes. CSRF tokens are not credentials.
CREATE TABLE server_identity (
 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
 server_id TEXT NOT NULL,
 -- Rotated by restore so event cursors from another database history reset.
 restore_epoch TEXT NOT NULL
);
INSERT INTO server_identity VALUES (1, lower(hex(randomblob(16))), lower(hex(randomblob(16))));

CREATE TABLE devices (
 id TEXT PRIMARY KEY,
 name TEXT NOT NULL,
 client_name TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision >= 1),
 profile_ids TEXT NOT NULL,
 permissions TEXT NOT NULL,
 policy TEXT NOT NULL,
 revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0, 1)),
 generation INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0),
 credential_expires_at INTEGER,
 created_at INTEGER NOT NULL
);

CREATE TABLE pairings (
 id TEXT PRIMARY KEY,
 device_code_hash TEXT NOT NULL UNIQUE,
 user_code TEXT NOT NULL,
 device_name TEXT NOT NULL,
 client_name TEXT NOT NULL,
 expires_at INTEGER NOT NULL,
 phase TEXT NOT NULL CHECK(phase IN ('pending', 'approved', 'claimed')),
 device_id TEXT REFERENCES devices(id),
 last_claim_at INTEGER,
 -- Re-derives the claimed credential for a lost-acknowledgement replay.
 credential_nonce TEXT,
 claimed_generation INTEGER,
 CHECK((phase = 'claimed') = (credential_nonce IS NOT NULL AND claimed_generation IS NOT NULL)),
 CHECK((phase = 'pending') = (device_id IS NULL))
);
CREATE INDEX pairings_expiry ON pairings(expires_at);

CREATE TABLE credentials (
 token_hash TEXT PRIMARY KEY,
 device_id TEXT NOT NULL REFERENCES devices(id),
 kind TEXT NOT NULL CHECK(kind IN ('device', 'access', 'session')),
 generation INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 -- Children (access, session) die with their parent device credential.
 parent_hash TEXT REFERENCES credentials(token_hash) ON DELETE CASCADE,
 csrf_token TEXT,
 CHECK((kind = 'session') = (csrf_token IS NOT NULL)),
 CHECK((kind = 'device') = (parent_hash IS NULL))
);
CREATE INDEX credentials_parent ON credentials(parent_hash);
CREATE INDEX credentials_device ON credentials(device_id);

CREATE TABLE idempotency_records (
 principal_id TEXT NOT NULL,
 operation TEXT NOT NULL,
 target TEXT NOT NULL,
 key TEXT NOT NULL,
 digest TEXT NOT NULL,
 status INTEGER NOT NULL,
 body TEXT,
 -- Nonce re-deriving the secret a secret-bearing acknowledgement issued.
 issued_nonce TEXT,
 expires_at INTEGER NOT NULL,
 PRIMARY KEY(principal_id, operation, target, key)
);
CREATE INDEX idempotency_expiry ON idempotency_records(expires_at);

ALTER TABLE change_events ADD COLUMN kind TEXT NOT NULL DEFAULT 'changed' CHECK(kind IN ('changed', 'deleted'));
DROP TRIGGER event_profiles_delete;
CREATE TRIGGER event_profiles_delete AFTER DELETE ON profiles BEGIN INSERT INTO change_events(topic,resource_id,kind) VALUES ('profiles',OLD.id,'deleted'); END;
DROP TRIGGER event_libraries_delete;
CREATE TRIGGER event_libraries_delete AFTER DELETE ON libraries BEGIN INSERT INTO change_events(topic,resource_id,kind) VALUES ('libraries',OLD.id,'deleted'); END;
CREATE TRIGGER event_devices_insert AFTER INSERT ON devices BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('devices',NEW.id); END;
CREATE TRIGGER event_devices_update AFTER UPDATE ON devices WHEN OLD.revision != NEW.revision OR OLD.name != NEW.name BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('devices',NEW.id); END;

-- Catalog hints name items. File mutations are normalized to their owning
-- item in the same transaction and record the library the file was in, so a
-- subscriber that could see the item there is reset when it leaves that
-- scope; item deletion is marked likewise.
ALTER TABLE change_events ADD COLUMN scope_library TEXT;
DROP TRIGGER event_media_files_insert;
DROP TRIGGER event_media_files_update;
DROP TRIGGER event_media_files_delete;
CREATE TRIGGER event_media_files_insert AFTER INSERT ON media_files BEGIN INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',item_id,NEW.library_id FROM editions WHERE id=NEW.edition_id; END;
CREATE TRIGGER event_media_files_update AFTER UPDATE ON media_files BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',item_id,OLD.library_id FROM editions WHERE id=OLD.edition_id;
 INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',item_id,NEW.library_id FROM editions WHERE id=NEW.edition_id AND (NEW.edition_id!=OLD.edition_id OR NEW.library_id!=OLD.library_id);
END;
CREATE TRIGGER event_media_files_delete AFTER DELETE ON media_files BEGIN INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'catalog',item_id,OLD.library_id FROM editions WHERE id=OLD.edition_id; END;
DROP TRIGGER event_items_delete;
CREATE TRIGGER event_items_delete AFTER DELETE ON items BEGIN INSERT INTO change_events(topic,resource_id,kind) VALUES ('catalog',OLD.id,'deleted'); END;

-- Content tickets pin one file revision and purpose. They die with the
-- credential that issued them (operator tickets have none and expire).
CREATE TABLE content_tickets (
 id TEXT PRIMARY KEY,
 token_hash TEXT NOT NULL UNIQUE,
 principal_id TEXT NOT NULL,
 credential_hash TEXT REFERENCES credentials(token_hash) ON DELETE CASCADE,
 file_id TEXT NOT NULL,
 file_revision TEXT NOT NULL,
 purpose TEXT NOT NULL CHECK(purpose IN ('playback','download')),
 expires_at INTEGER NOT NULL,
 revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1))
);
CREATE INDEX content_tickets_expiry ON content_tickets(expires_at);
