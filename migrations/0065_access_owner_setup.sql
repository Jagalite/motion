-- First-run owner setup. At most one row records the user created by guided
-- setup; it is inserted in the same transaction as that user and never replaced.
-- Existing installations keep their users and devices unchanged: setup does
-- not adopt an existing user or assign any device.
CREATE TABLE access_owner_setup (
 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
 user_id TEXT NOT NULL UNIQUE REFERENCES access_users(id),
 created_at INTEGER NOT NULL
);
