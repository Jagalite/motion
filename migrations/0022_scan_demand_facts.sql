-- Facts for shared scan demands (A03 review):
-- * jobs.direct_request: an attempt admitted by a direct requester (v1 API,
--   schedules, configured roots). Cancelling a demand never stops it.
-- * scan_demands coverage summary is retained after the answering job is
--   pruned from history.
ALTER TABLE jobs ADD COLUMN direct_request INTEGER NOT NULL DEFAULT 1;
ALTER TABLE scan_demands ADD COLUMN complete_directories INTEGER NOT NULL DEFAULT 0;
ALTER TABLE scan_demands ADD COLUMN incomplete_directories INTEGER NOT NULL DEFAULT 0;
