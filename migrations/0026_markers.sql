-- Timeline markers (A04). Positions are logical timeline milliseconds; each
-- marker records the timeline revision it was validated against.
CREATE TABLE markers (
 id TEXT PRIMARY KEY,
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision >= 1),
 timeline_id TEXT NOT NULL REFERENCES timelines(id) ON DELETE CASCADE,
 timeline_revision INTEGER NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('chapter','intro','credits','recap')),
 start_ms INTEGER NOT NULL CHECK(start_ms >= 0),
 end_ms INTEGER CHECK(end_ms IS NULL OR end_ms > start_ms),
 label TEXT,
 provenance TEXT NOT NULL CHECK(provenance IN ('embedded','manual','detected','imported'))
);
CREATE INDEX markers_timeline ON markers(timeline_id,start_ms,id);
CREATE TRIGGER event_markers_insert AFTER INSERT ON markers BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('markers',NEW.timeline_id); END;
CREATE TRIGGER event_markers_delete AFTER DELETE ON markers BEGIN INSERT INTO change_events(topic,resource_id) VALUES ('markers',OLD.timeline_id); END;
