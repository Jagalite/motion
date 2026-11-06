-- Expiring reviewed proposals are not immutable committed audit receipts.
-- Keep opaque large-plan references in a separate namespace so expiry cleanup
-- cannot delete historical mutation results.
CREATE TABLE catalog_review_plans (
 id TEXT PRIMARY KEY,
 principal_id TEXT NOT NULL,
 expires_at INTEGER NOT NULL,
 document_json TEXT NOT NULL CHECK(length(CAST(document_json AS BLOB))<=8388608)
);
CREATE INDEX catalog_review_plans_expiry ON catalog_review_plans(expires_at);
