-- Completion 6B: the chapters a media file's container records, as probed
-- by the scanner (JSON array of {start_ms, end_ms, title}). NULL until the
-- file is probed for chapters; an empty array when it has none. Chapter
-- markers are derived from it at read time (markers::embedded_chapters), so
-- they always belong to the timeline's current revision.
ALTER TABLE media_files ADD COLUMN chapters_json TEXT;
