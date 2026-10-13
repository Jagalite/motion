-- Historical ownership must survive deletion and moves; it is not an FK.
-- NULL marks older unattributed events, handled by anonymous resets in core.
ALTER TABLE change_events ADD COLUMN scope_profile TEXT;

DROP TRIGGER event_collections_insert;
DROP TRIGGER event_collections_change;
DROP TRIGGER event_collections_delete;
CREATE TRIGGER event_collections_insert AFTER INSERT ON collections BEGIN
 INSERT INTO change_events(topic,resource_id,scope_profile) VALUES ('organization',NEW.id,NEW.profile_id); END;
CREATE TRIGGER event_collections_change AFTER UPDATE ON collections BEGIN
 INSERT INTO change_events(topic,resource_id,scope_profile) VALUES ('organization',NEW.id,OLD.profile_id); END;
CREATE TRIGGER event_collections_delete AFTER DELETE ON collections BEGIN
 INSERT INTO change_events(topic,resource_id,kind,scope_profile) VALUES ('organization',OLD.id,'deleted',OLD.profile_id); END;

DROP TRIGGER event_saved_filters_insert;
DROP TRIGGER event_saved_filters_change;
DROP TRIGGER event_saved_filters_delete;
CREATE TRIGGER event_saved_filters_insert AFTER INSERT ON saved_filters BEGIN
 INSERT INTO change_events(topic,resource_id,scope_profile) VALUES ('organization',NEW.id,NEW.profile_id); END;
CREATE TRIGGER event_saved_filters_change AFTER UPDATE ON saved_filters BEGIN
 INSERT INTO change_events(topic,resource_id,scope_profile) VALUES ('organization',NEW.id,OLD.profile_id); END;
CREATE TRIGGER event_saved_filters_delete AFTER DELETE ON saved_filters BEGIN
 INSERT INTO change_events(topic,resource_id,kind,scope_profile) VALUES ('organization',OLD.id,'deleted',OLD.profile_id); END;

DROP TRIGGER event_playlists_insert;
DROP TRIGGER event_playlists_change;
DROP TRIGGER event_playlists_delete;
CREATE TRIGGER event_playlists_insert AFTER INSERT ON playlists BEGIN
 INSERT INTO change_events(topic,resource_id,scope_profile) VALUES ('organization',NEW.id,NEW.profile_id); END;
CREATE TRIGGER event_playlists_change AFTER UPDATE ON playlists BEGIN
 INSERT INTO change_events(topic,resource_id,scope_profile) VALUES ('organization',NEW.id,OLD.profile_id); END;
CREATE TRIGGER event_playlists_delete AFTER DELETE ON playlists BEGIN
 INSERT INTO change_events(topic,resource_id,kind,scope_profile) VALUES ('organization',OLD.id,'deleted',OLD.profile_id); END;

DROP TRIGGER api_schedule_insert;
DROP TRIGGER api_schedule_update;
DROP TRIGGER api_schedule_delete;
CREATE TRIGGER api_schedule_insert AFTER INSERT ON api_schedules BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) VALUES ('schedules',NEW.id,NEW.library_id); END;
CREATE TRIGGER api_schedule_update AFTER UPDATE ON api_schedules
WHEN OLD.revision IS NOT NEW.revision OR OLD.library_id IS NOT NEW.library_id BEGIN
 INSERT INTO change_events(topic,resource_id,scope_library) VALUES ('schedules',NEW.id,OLD.library_id); END;
CREATE TRIGGER api_schedule_delete AFTER DELETE ON api_schedules BEGIN
 INSERT INTO change_events(topic,resource_id,kind,scope_library) VALUES ('schedules',OLD.id,'deleted',OLD.library_id); END;

-- Physical legacy schedule IDs cannot be confused with logical API schedules.
DROP TRIGGER event_scan_schedules_insert;
DROP TRIGGER event_scan_schedules_update;
DROP TRIGGER event_scan_schedules_delete;
CREATE TRIGGER event_scan_schedules_insert AFTER INSERT ON scan_schedules BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('legacy_scan_schedules',NEW.library_id); END;
CREATE TRIGGER event_scan_schedules_update AFTER UPDATE ON scan_schedules BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('legacy_scan_schedules',NEW.library_id); END;
CREATE TRIGGER event_scan_schedules_delete AFTER DELETE ON scan_schedules BEGIN
 INSERT INTO change_events(topic,resource_id,kind) VALUES ('legacy_scan_schedules',OLD.library_id,'deleted'); END;
