-- Disposable bridge between preserved Motion reference revisions and current
-- native typed identity. Source revision numbers are never native revisions.
CREATE TABLE catabolic_projection_versions (
 version_id TEXT PRIMARY KEY,
 motion_version_revision INTEGER NOT NULL,
 timeline_id TEXT NOT NULL,
 motion_timeline_revision INTEGER NOT NULL,
 native_version_revision INTEGER NOT NULL,
 native_timeline_revision INTEGER NOT NULL,
 native_current_timeline_revision INTEGER NOT NULL,
 generation INTEGER NOT NULL,
 ready INTEGER NOT NULL CHECK(ready IN (0,1)),
 manifest_json TEXT NOT NULL CHECK(json_valid(manifest_json))
);
CREATE TABLE catabolic_projection_files (
 file_id TEXT PRIMARY KEY,
 native_profile TEXT NOT NULL,
 native_file_id TEXT NOT NULL,
 native_revision TEXT NOT NULL,
 generation INTEGER NOT NULL,
 available INTEGER NOT NULL CHECK(available IN (0,1)),
 mapping_json TEXT NOT NULL CHECK(json_valid(mapping_json))
);
