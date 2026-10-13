CREATE TABLE signin_settings (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL DEFAULT 1,
 public_origin TEXT, reviewed_epoch TEXT NOT NULL, email_json TEXT NOT NULL DEFAULT '{}',
 email_secret TEXT, email_revision INTEGER NOT NULL DEFAULT 1,
 email_test_at INTEGER, email_test_ok INTEGER
);
INSERT INTO signin_settings(singleton,reviewed_epoch) SELECT 1,restore_epoch FROM server_identity;
CREATE TABLE signin_providers (
 id TEXT PRIMARY KEY, revision INTEGER NOT NULL, name TEXT NOT NULL,
 issuer TEXT NOT NULL UNIQUE, client_id TEXT NOT NULL, secret TEXT,
 destinations TEXT NOT NULL DEFAULT '[]', enabled INTEGER NOT NULL DEFAULT 0,
 test_at INTEGER, test_ok INTEGER, login_test_at INTEGER
);
CREATE TABLE signin_identities (
 id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES access_users(id), kind TEXT NOT NULL CHECK(kind IN ('oidc','email')),
 provider_id TEXT REFERENCES signin_providers(id), issuer TEXT, subject TEXT, address TEXT,
 enabled INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 1,
 profile_ids TEXT NOT NULL, permissions TEXT NOT NULL, policy TEXT NOT NULL, created_at INTEGER NOT NULL,
 UNIQUE(issuer,subject), UNIQUE(address)
);
CREATE TABLE signin_families (
 id TEXT PRIMARY KEY, device_id TEXT NOT NULL UNIQUE REFERENCES devices(id), user_id TEXT NOT NULL REFERENCES access_users(id),
 identity_id TEXT NOT NULL REFERENCES signin_identities(id), identity_revision INTEGER NOT NULL, method_revision INTEGER NOT NULL,
 epoch TEXT NOT NULL, issued_at INTEGER NOT NULL, authenticated_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
 revoked INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1,
 parent_hash TEXT NOT NULL, session_hash TEXT NOT NULL UNIQUE, last_seen_at INTEGER
);
CREATE TABLE signin_transactions (
 id TEXT PRIMARY KEY, state_hash TEXT UNIQUE, provider_id TEXT REFERENCES signin_providers(id),
 state_json TEXT NOT NULL, nonce TEXT, verifier TEXT, address TEXT, code_hash TEXT, ceiling_json TEXT,
 return_to TEXT NOT NULL, actor_id TEXT, actor_credential_hash TEXT, identity_id TEXT,
 created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, result TEXT
);
CREATE INDEX signin_tx_expiry ON signin_transactions(expires_at);
CREATE TABLE signin_requests (
 id TEXT PRIMARY KEY, provider_id TEXT NOT NULL REFERENCES signin_providers(id), provider_revision INTEGER NOT NULL,
 issuer TEXT NOT NULL, subject TEXT NOT NULL, email TEXT, name TEXT,
 status TEXT NOT NULL DEFAULT 'pending', revision INTEGER NOT NULL DEFAULT 1,
 created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, user_id TEXT REFERENCES access_users(id)
);
CREATE UNIQUE INDEX signin_pending_subject ON signin_requests(issuer,subject) WHERE status='pending';
CREATE TABLE signin_proofs (
 id TEXT PRIMARY KEY, actor_id TEXT NOT NULL, actor_credential_hash TEXT,
 action TEXT NOT NULL, target TEXT NOT NULL, epoch TEXT NOT NULL,
 code_hash TEXT NOT NULL, expires_at INTEGER NOT NULL, approved_by TEXT, approver_credential_hash TEXT,
 consumed INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL
);
CREATE TABLE signin_audit (id INTEGER PRIMARY KEY AUTOINCREMENT,actor TEXT NOT NULL,target TEXT NOT NULL,action TEXT NOT NULL,outcome TEXT NOT NULL,at INTEGER NOT NULL);
CREATE TRIGGER bound_signin_audit AFTER INSERT ON signin_audit BEGIN DELETE FROM signin_audit WHERE id <= NEW.id-10000; END;
CREATE TABLE signin_limits (bucket TEXT PRIMARY KEY,window_start INTEGER NOT NULL,attempts INTEGER NOT NULL);
CREATE TABLE signin_outbox (
 id TEXT PRIMARY KEY, transaction_id TEXT REFERENCES signin_transactions(id) ON DELETE CASCADE,
 payload TEXT, phase TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0,
 next_attempt INTEGER NOT NULL, expires_at INTEGER NOT NULL, config_revision INTEGER NOT NULL,
 destination_hash TEXT NOT NULL, created_at INTEGER NOT NULL
);
