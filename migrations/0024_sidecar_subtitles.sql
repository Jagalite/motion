-- Sidecar subtitle observations per media file (A04 component evidence).
-- Replaced as a set whenever the media file is published by a scan.
CREATE TABLE sidecar_subtitles (
 file_id TEXT NOT NULL REFERENCES media_files(id),
 name TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 language TEXT,
 forced INTEGER NOT NULL CHECK(forced IN (0,1)),
 hearing_impaired INTEGER NOT NULL CHECK(hearing_impaired IN (0,1)),
 format TEXT NOT NULL CHECK(format IN ('srt','vtt','ass','ssa')),
 PRIMARY KEY(file_id,name)
);
