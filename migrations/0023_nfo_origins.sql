-- Which file's sidecar supplied a work's `nfo` contribution, so removing that
-- sidecar withdraws the contribution (absence withdraws a source's opinion)
-- while copies without sidecars do not.
CREATE TABLE nfo_origins (
 item_id TEXT PRIMARY KEY REFERENCES items(id),
 file_id TEXT NOT NULL REFERENCES media_files(id)
);
