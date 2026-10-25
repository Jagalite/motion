-- Saved native query references are prepared by the trusted integration owner,
-- never supplied by a browser caller. Each append binds the exact source map
-- and logical library revision; changed membership invalidates older rows.
CREATE TABLE catabolic_server_scopes (
 profile TEXT NOT NULL,
 library_id TEXT NOT NULL,
 library_revision INTEGER NOT NULL CHECK(library_revision>=0),
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 source_digest TEXT NOT NULL,
 source_mappings_json TEXT NOT NULL CHECK(json_valid(source_mappings_json)),
 query_id TEXT NOT NULL,
 query_revision INTEGER NOT NULL CHECK(query_revision>0),
 query_digest TEXT NOT NULL,
 created_at INTEGER NOT NULL,
 PRIMARY KEY(profile,library_id,library_revision,source_digest),
 CHECK(length(source_digest)=64),
 CHECK(length(query_digest)=64)
);
CREATE TRIGGER catabolic_server_scopes_immutable_update BEFORE UPDATE ON catabolic_server_scopes
 BEGIN SELECT RAISE(ABORT,'native server scopes are immutable'); END;
CREATE TRIGGER catabolic_server_scopes_immutable_delete BEFORE DELETE ON catabolic_server_scopes
 BEGIN SELECT RAISE(ABORT,'native server scopes are immutable'); END;
