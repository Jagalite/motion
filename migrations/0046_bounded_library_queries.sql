-- Bounded library queries (completion item 2): newest-first scan history per
-- library and overall, and renditions by timeline. Indexes only; settled scan
-- history is pruned by playscale_core::demands::prunable_requests at admission.
CREATE INDEX scan_requests_recent ON scan_requests(created_at DESC,id DESC);
CREATE INDEX scan_requests_library_recent ON scan_requests(library_id,created_at DESC,id DESC);
CREATE INDEX preparations_timeline ON preparations(timeline_id,job_id);
