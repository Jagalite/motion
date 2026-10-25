-- Exact Motion-owned preview output ACK. Existing rows deliberately have no
-- inferred artifact identity and are not native-ready until rebuilt.
CREATE TABLE preview_sheet_artifacts (
 file_id TEXT NOT NULL,
 file_revision TEXT NOT NULL,
 sheet INTEGER NOT NULL CHECK(sheet BETWEEN 1 AND 20),
 layout_identity TEXT NOT NULL,
 digest TEXT NOT NULL CHECK(length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'),
 size_bytes INTEGER NOT NULL CHECK(size_bytes BETWEEN 1 AND 4194304),
 PRIMARY KEY(file_id,file_revision,sheet),
 FOREIGN KEY(file_id,file_revision) REFERENCES previews(file_id,file_revision) ON DELETE CASCADE
);
