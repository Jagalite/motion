-- Native direct mail delivery, durable delivery attempts and email registration.
-- Existing deployments keep relay delivery and closed registration until an
-- administrator explicitly selects otherwise.
ALTER TABLE signin_settings ADD COLUMN registration_json TEXT NOT NULL DEFAULT '{"policy":"closed"}';
ALTER TABLE signin_settings ADD COLUMN registration_revision INTEGER NOT NULL DEFAULT 1;
-- Sealed PKCS#8 DKIM private key and its public SubjectPublicKeyInfo (base64).
ALTER TABLE signin_settings ADD COLUMN dkim_secret TEXT;
ALTER TABLE signin_settings ADD COLUMN dkim_public TEXT;
ALTER TABLE signin_settings ADD COLUMN email_receipt_at INTEGER;

-- Rebuild the outbox: records outlive their challenge (bounded activity) and
-- carry attempt identity, lease, ambiguity and configuration/key/epoch fences.
CREATE TABLE signin_outbox_next (
 id TEXT PRIMARY KEY,
 purpose TEXT NOT NULL CHECK(purpose IN ('signin_code','registration_code','invitation','account_notice','test')),
 transaction_id TEXT REFERENCES signin_transactions(id) ON DELETE SET NULL,
 owner_id TEXT NOT NULL,
 payload TEXT,
 phase TEXT NOT NULL CHECK(phase IN ('queued','sending','accepted','failed','unknown','expired','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0,
 next_attempt INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('relay','direct')),
 config_revision INTEGER NOT NULL,
 key_id TEXT,
 epoch TEXT NOT NULL,
 lease_token TEXT,
 lease_attempt INTEGER,
 lease_until INTEGER,
 ambiguous INTEGER NOT NULL DEFAULT 0 CHECK(ambiguous IN (0,1)),
 last_error TEXT,
 last_attempt_at INTEGER,
 accepted_at INTEGER,
 recipient_domain TEXT NOT NULL,
 destination_hash TEXT NOT NULL,
 message_hash TEXT,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 CHECK((phase='sending') = (lease_token IS NOT NULL)),
 CHECK(phase IN ('queued','sending') OR payload IS NULL)
);
INSERT INTO signin_outbox_next(id,purpose,transaction_id,owner_id,payload,phase,attempts,next_attempt,expires_at,mode,config_revision,key_id,epoch,lease_token,lease_attempt,lease_until,ambiguous,recipient_domain,destination_hash,created_at,updated_at)
SELECT o.id,'signin_code',o.transaction_id,COALESCE(o.transaction_id,o.id),
 CASE WHEN o.phase IN ('pending','sending') THEN o.payload END,
 CASE o.phase WHEN 'pending' THEN 'queued' WHEN 'sending' THEN 'sending' WHEN 'sent' THEN 'accepted' WHEN 'expired' THEN 'expired' ELSE 'cancelled' END,
 o.attempts,o.next_attempt,o.expires_at,'relay',o.config_revision,NULL,
 COALESCE((SELECT restore_epoch FROM server_identity),''),
 -- An interrupted legacy attempt is recovered as ambiguous by an expired lease.
 CASE WHEN o.phase='sending' THEN 'legacy' END,
 CASE WHEN o.phase='sending' THEN o.attempts END,
 CASE WHEN o.phase='sending' THEN 0 END,
 0,'unknown',o.destination_hash,o.created_at,o.created_at
FROM signin_outbox o;
DROP TABLE signin_outbox;
ALTER TABLE signin_outbox_next RENAME TO signin_outbox;
CREATE INDEX signin_outbox_due ON signin_outbox(phase,next_attempt);
CREATE INDEX signin_outbox_owner ON signin_outbox(owner_id);
CREATE INDEX signin_outbox_created ON signin_outbox(created_at);

-- Email access requests join OIDC requests. An address is never a linking key;
-- approval explicitly selects a user and grant.
CREATE TABLE signin_requests_next (
 id TEXT PRIMARY KEY,
 kind TEXT NOT NULL DEFAULT 'oidc' CHECK(kind IN ('oidc','email')),
 provider_id TEXT REFERENCES signin_providers(id), provider_revision INTEGER,
 issuer TEXT, subject TEXT, email TEXT, name TEXT, address TEXT,
 status TEXT NOT NULL DEFAULT 'pending', revision INTEGER NOT NULL DEFAULT 1,
 created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, user_id TEXT REFERENCES access_users(id),
 CHECK((kind='oidc') = (provider_id IS NOT NULL AND provider_revision IS NOT NULL AND issuer IS NOT NULL AND subject IS NOT NULL AND address IS NULL)),
 CHECK((kind='email') = (address IS NOT NULL AND provider_id IS NULL))
);
INSERT INTO signin_requests_next(id,kind,provider_id,provider_revision,issuer,subject,email,name,status,revision,created_at,expires_at,user_id)
SELECT id,'oidc',provider_id,provider_revision,issuer,subject,email,name,status,revision,created_at,expires_at,user_id FROM signin_requests;
DROP TABLE signin_requests;
ALTER TABLE signin_requests_next RENAME TO signin_requests;
CREATE UNIQUE INDEX signin_pending_subject ON signin_requests(issuer,subject) WHERE status='pending' AND kind='oidc';
CREATE UNIQUE INDEX signin_pending_address ON signin_requests(address) WHERE status='pending' AND kind='email';

-- Email-bound invitations. Completion by verified email records the identity;
-- device redemption and email completion are mutually exclusive claims.
ALTER TABLE access_invitations ADD COLUMN email TEXT;
ALTER TABLE access_invitations ADD COLUMN identity_id TEXT REFERENCES signin_identities(id);
CREATE INDEX access_invitations_email ON access_invitations(email) WHERE email IS NOT NULL;

-- Registration name chosen by the registrant (open policy display name only).
ALTER TABLE signin_transactions ADD COLUMN registration_name TEXT;

-- Explicit administrator delivery tests. Only a keyed code digest is stored;
-- entering the code proves receipt for the tested configuration revision.
CREATE TABLE signin_mail_tests (
 id TEXT PRIMARY KEY, actor_id TEXT NOT NULL, config_revision INTEGER NOT NULL,
 mode TEXT NOT NULL, recipient_domain TEXT NOT NULL, code_hash TEXT,
 attempts INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL, confirmed_at INTEGER
);

-- Open-registration activations for the rolling daily bound. Separate from the
-- bounded audit log so public traffic cannot evict entries inside the window.
CREATE TABLE registration_activations (user_id TEXT NOT NULL, at INTEGER NOT NULL);
CREATE INDEX registration_activations_at ON registration_activations(at);
