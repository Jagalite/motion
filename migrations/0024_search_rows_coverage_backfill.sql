-- Review follow-ups for 0019/0020.
-- 1. FTS documents are deleted through an indexed item -> FTS rowid map
--    (item_id is UNINDEXED in search_fts). Items' implicit rowids are not used
--    because VACUUM may renumber them. All documents are rebuilt.
CREATE TABLE search_rows (
 item_id TEXT PRIMARY KEY,
 fts_rowid INTEGER NOT NULL UNIQUE
);
DELETE FROM search_fts;
INSERT INTO search_dirty SELECT id FROM items WHERE NOT EXISTS (SELECT 1 FROM search_dirty d WHERE d.item_id=items.id);
-- 2. Demands resolved before 0020 keep the coverage their answering job reported.
UPDATE scan_demands SET
 complete_directories=coalesce((SELECT complete_directories FROM jobs WHERE jobs.id=scan_demands.job_id),complete_directories),
 incomplete_directories=coalesce((SELECT incomplete_directories FROM jobs WHERE jobs.id=scan_demands.job_id),incomplete_directories)
 WHERE job_id IS NOT NULL;
