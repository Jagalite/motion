-- Identification workflow. Proposals are evidence plus an operator decision,
-- fenced on their own revision and the exact file revision reviewed.
ALTER TABLE items ADD COLUMN match_state TEXT NOT NULL DEFAULT 'unmatched'
 CHECK(match_state IN ('unmatched','ambiguous','matched','manual'));
CREATE TABLE match_proposals (
 id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL CHECK(revision >= 1),
 file_id TEXT NOT NULL REFERENCES media_files(id),
 file_revision TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('pending','review','accepted','rejected','deferred','stale')),
 candidates_json TEXT NOT NULL,
 decision_json TEXT,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL
);
-- At most one undecided proposal per file; decided ones are retained history.
CREATE UNIQUE INDEX match_one_open ON match_proposals(file_id) WHERE status IN ('pending','review','deferred');
CREATE INDEX match_inbox ON match_proposals(status,updated_at,id);
CREATE TRIGGER event_match_proposals_insert AFTER INSERT ON match_proposals BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('matches',NEW.id); END;
CREATE TRIGGER event_match_proposals_update AFTER UPDATE ON match_proposals BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('matches',NEW.id); END;
