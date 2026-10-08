ALTER TABLE libraries ADD COLUMN managed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE media_files ADD COLUMN generated INTEGER NOT NULL DEFAULT 0;
CREATE TABLE processing_jobs (
 id TEXT PRIMARY KEY, source_file_id TEXT NOT NULL REFERENCES media_files(id), source_revision TEXT NOT NULL,
 recipe TEXT NOT NULL, backend TEXT NOT NULL, idempotency_key TEXT NOT NULL UNIQUE,
 phase TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0, progress_seconds REAL NOT NULL DEFAULT 0,
 output_file_id TEXT REFERENCES media_files(id), error TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
 expired INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX processing_queue ON processing_jobs(phase,created_at,id);
CREATE TABLE scan_schedules (library_id TEXT PRIMARY KEY REFERENCES libraries(id), interval_seconds INTEGER NOT NULL, next_run INTEGER NOT NULL);
CREATE TABLE change_events (id INTEGER PRIMARY KEY AUTOINCREMENT, topic TEXT NOT NULL, resource_id TEXT NOT NULL);
CREATE TRIGGER bound_events AFTER INSERT ON change_events BEGIN
 DELETE FROM change_events WHERE id <= NEW.id - 10000;
END;
CREATE TRIGGER event_items_insert AFTER INSERT ON items BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('catalog',NEW.id); END;
CREATE TRIGGER event_items_update AFTER UPDATE ON items BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('catalog',NEW.id); END;
CREATE TRIGGER event_items_delete AFTER DELETE ON items BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('catalog',OLD.id); END;
CREATE TRIGGER event_media_files_insert AFTER INSERT ON media_files BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('catalog',NEW.id); END;
CREATE TRIGGER event_media_files_update AFTER UPDATE ON media_files BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('catalog',NEW.id); END;
CREATE TRIGGER event_media_files_delete AFTER DELETE ON media_files BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('catalog',OLD.id); END;
CREATE TRIGGER event_jobs_insert AFTER INSERT ON jobs BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('scan',NEW.id); END;
CREATE TRIGGER event_jobs_update AFTER UPDATE ON jobs BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('scan',NEW.id); END;
CREATE TRIGGER event_jobs_delete AFTER DELETE ON jobs BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('scan',OLD.id); END;
CREATE TRIGGER event_processing_jobs_insert AFTER INSERT ON processing_jobs BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('processing',NEW.id); END;
CREATE TRIGGER event_processing_jobs_update AFTER UPDATE ON processing_jobs BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('processing',NEW.id); END;
CREATE TRIGGER event_processing_jobs_delete AFTER DELETE ON processing_jobs BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('processing',OLD.id); END;
CREATE TRIGGER event_playback_sessions_insert AFTER INSERT ON playback_sessions BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('playback',NEW.profile_id); END;
CREATE TRIGGER event_playback_sessions_update AFTER UPDATE ON playback_sessions BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('playback',NEW.profile_id); END;
CREATE TRIGGER event_playback_sessions_delete AFTER DELETE ON playback_sessions BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('playback',OLD.profile_id); END;
CREATE TRIGGER event_viewing_state_insert AFTER INSERT ON viewing_state BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('viewing',NEW.profile_id); END;
CREATE TRIGGER event_viewing_state_update AFTER UPDATE ON viewing_state BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('viewing',NEW.profile_id); END;
CREATE TRIGGER event_viewing_state_delete AFTER DELETE ON viewing_state BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('viewing',OLD.profile_id); END;
CREATE TRIGGER event_playback_preferences_insert AFTER INSERT ON playback_preferences BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('preferences',NEW.profile_id); END;
CREATE TRIGGER event_playback_preferences_update AFTER UPDATE ON playback_preferences BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('preferences',NEW.profile_id); END;
CREATE TRIGGER event_playback_preferences_delete AFTER DELETE ON playback_preferences BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('preferences',OLD.profile_id); END;
