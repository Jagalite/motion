-- Persist fair cleanup progress independently of immutable backup provenance.
-- Failed removals remain withdrawn and are revisited after the cursor wraps.
CREATE TABLE backup_cleanup_cursor (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 after_id TEXT NOT NULL DEFAULT ''
);
INSERT INTO backup_cleanup_cursor(singleton,after_id) VALUES (1,'');
