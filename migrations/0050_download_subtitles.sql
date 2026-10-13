-- Completion 5E part 4: the text subtitle track of the pinned file saved
-- beside a download as WebVTT (core downloads::choose); NULL for none.
ALTER TABLE downloads ADD COLUMN subtitle_track_id TEXT;
