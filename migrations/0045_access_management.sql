-- Keep device principals and exact legacy grants. Each legacy device receives
-- its own explicitly marked identity; equal display names never imply ownership.
CREATE TABLE access_users (
 id TEXT PRIMARY KEY,
 name TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>=1),
 disabled INTEGER NOT NULL DEFAULT 0 CHECK(disabled IN (0,1)),
 profile_ids TEXT NOT NULL,
 permissions TEXT NOT NULL,
 policy TEXT NOT NULL,
 legacy INTEGER NOT NULL DEFAULT 0 CHECK(legacy IN (0,1)),
 created_at INTEGER NOT NULL
);
INSERT INTO access_users SELECT 'user-'||id,CASE WHEN length(name)>85 THEN substr('Legacy device: '||name,1,99)||'…' ELSE 'Legacy device: '||name END,1,revoked,profile_ids,permissions,policy,1,created_at FROM devices;
ALTER TABLE devices ADD COLUMN user_id TEXT REFERENCES access_users(id);
UPDATE devices SET user_id='user-'||id;
CREATE INDEX devices_user ON devices(user_id);
-- The existing phase CHECK is retained; denial is an explicit orthogonal fact.
ALTER TABLE pairings ADD COLUMN denied INTEGER NOT NULL DEFAULT 0 CHECK(denied IN (0,1));
ALTER TABLE pairings ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
UPDATE pairings SET created_at=expires_at-600;

CREATE TABLE access_invitations (
 id TEXT PRIMARY KEY,
 code_hash TEXT NOT NULL UNIQUE,
 user_id TEXT NOT NULL REFERENCES access_users(id),
 user_revision INTEGER NOT NULL,
 issuer_id TEXT NOT NULL,
 issuer_revision INTEGER NOT NULL,
 issuer_credential_hash TEXT,
 profile_ids TEXT NOT NULL,
 permissions TEXT NOT NULL,
 policy TEXT NOT NULL,
 created_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 revision INTEGER NOT NULL DEFAULT 1,
 revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1)),
 claim_hash TEXT,
 device_id TEXT REFERENCES devices(id),
 generation INTEGER,
 credential_nonce TEXT,
 CHECK((device_id IS NULL) = (claim_hash IS NULL)),
 CHECK((device_id IS NULL) = (credential_nonce IS NULL)),
 CHECK((device_id IS NULL) = (generation IS NULL))
);
CREATE TABLE invitation_attempts (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 window_start INTEGER NOT NULL,
 attempts INTEGER NOT NULL
);
INSERT INTO invitation_attempts VALUES(1,0,0);
