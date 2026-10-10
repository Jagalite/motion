-- Occurrence lookup: which files hold a given content revision.
CREATE INDEX media_files_revision ON media_files(revision);
