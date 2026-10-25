-- Disposable exact native physical-library membership, never legacy scope.
CREATE TABLE catabolic_projection_file_libraries (
 file_id TEXT NOT NULL,
 library_id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation>=0),
 proof_json TEXT NOT NULL CHECK(json_valid(proof_json) AND json_extract(proof_json,'$.contract_version')=1),
 PRIMARY KEY(file_id,library_id)
);
CREATE TRIGGER catabolic_projection_file_libraries_insert BEFORE INSERT ON catabolic_projection_file_libraries
 WHEN motion_native_projection('catabolic_projection_file_libraries')<>1
 BEGIN SELECT RAISE(ABORT,'native library projection requires isolated owner'); END;
CREATE TRIGGER catabolic_projection_file_libraries_update BEFORE UPDATE ON catabolic_projection_file_libraries
 WHEN motion_native_projection('catabolic_projection_file_libraries')<>1
 BEGIN SELECT RAISE(ABORT,'native library projection requires isolated owner'); END;
CREATE TRIGGER catabolic_projection_file_libraries_delete BEFORE DELETE ON catabolic_projection_file_libraries
 WHEN motion_native_projection('catabolic_projection_file_libraries')<>1
 BEGIN SELECT RAISE(ABORT,'native library projection requires isolated owner'); END;
