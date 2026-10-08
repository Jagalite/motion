-- Immutable parameters travel with each attempt and survive retries/restarts.
ALTER TABLE processing_jobs ADD COLUMN video_profile TEXT;
