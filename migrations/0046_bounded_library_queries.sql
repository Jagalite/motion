-- Bounded library queries (completion item 2): scan history newest first by
-- admission sequence (rowid), per library, and renditions by timeline.
-- Indexes only; settled scan history is pruned by
-- playscale_core::demands::prunable_requests at admission.
CREATE INDEX scan_requests_library ON scan_requests(library_id);
CREATE INDEX preparations_timeline ON preparations(timeline_id,job_id);
