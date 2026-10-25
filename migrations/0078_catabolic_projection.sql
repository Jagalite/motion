-- Disposable generation-tagged read model. These rows never establish native
-- mutation authority, source bindings, access grants or activation proof.
CREATE TABLE catabolic_projection_state (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation>=0),
 model_version INTEGER NOT NULL CHECK(model_version=1)
);
CREATE TABLE catabolic_projection_items (
 native_id TEXT PRIMARY KEY,
 native_revision INTEGER NOT NULL,
 metadata_json TEXT NOT NULL CHECK(json_valid(metadata_json)),
 generation INTEGER NOT NULL CHECK(generation>=0)
);
