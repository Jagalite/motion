-- A07 migration allocation request for A02: 0019 is already in use by A08.
-- Keep receipts independent of diagnostic retention. A retired delivery's exact
-- acknowledgement must replay without recreating execution. No receipt expiry
-- is applied until a policy can prove termination plus the required retention.
CREATE TABLE delivery_admissions (
 principal TEXT NOT NULL,
 request_key TEXT NOT NULL,
 request_digest TEXT NOT NULL,
 delivery_id TEXT NOT NULL UNIQUE,
 acknowledgement_json TEXT NOT NULL,
 created_at INTEGER NOT NULL,
 PRIMARY KEY(principal, request_key)
);
