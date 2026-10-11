-- Persist bounded metadata fanout across maintenance ticks. Replacing a schedule
-- restarts its next cycle; deleting it removes only dispatch progress, not jobs.
CREATE TABLE schedule_dispatch_progress (
 schedule_id TEXT PRIMARY KEY REFERENCES api_schedules(id) ON DELETE CASCADE,
 after_item TEXT NOT NULL
);
CREATE TRIGGER schedule_progress_reset AFTER UPDATE OF revision ON api_schedules BEGIN
 DELETE FROM schedule_dispatch_progress WHERE schedule_id=NEW.id; END;
