-- Data/catalog owns completion adapter state. Never rewrite prior migrations.
CREATE TABLE preparations (
 job_id TEXT PRIMARY KEY REFERENCES processing_jobs(id),
 timeline_id TEXT NOT NULL REFERENCES timelines(id),
 recipe_digest TEXT NOT NULL,
 tracks_json TEXT NOT NULL,
 credential_hash TEXT,
 ingress_id TEXT
);
