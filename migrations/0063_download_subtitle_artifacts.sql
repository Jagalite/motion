-- Facts of the required subtitle artifact, published only for an active demand.
CREATE TABLE download_subtitle_artifacts (
    download_id TEXT PRIMARY KEY REFERENCES downloads(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('ready','failed')),
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    digest TEXT,
    CHECK ((state='ready' AND size_bytes>0 AND length(digest)=64) OR (state='failed' AND size_bytes=0 AND digest IS NULL))
);
