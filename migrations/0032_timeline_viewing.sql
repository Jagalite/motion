-- A07: timeline-keyed viewing authority for /api/v2 (playscale_core::timeline_viewing).
-- Number 0032 allocated by the A02/PR #4 owner. Legacy item-keyed rows
-- (viewing_state, progress, playback_sessions) are retained unchanged.

CREATE TABLE timeline_viewing (
 profile_id TEXT NOT NULL REFERENCES profiles(id),
 -- No foreign key: viewing history must not block catalog edits; reads join
 -- through timelines and skip rows whose timeline no longer exists.
 timeline_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision >= 0),
 manual_epoch INTEGER NOT NULL DEFAULT 0 CHECK(manual_epoch >= 0),
 position_ms INTEGER NOT NULL DEFAULT 0 CHECK(position_ms >= 0),
 automatic_watched INTEGER NOT NULL DEFAULT 0 CHECK(automatic_watched IN (0,1)),
 manual_watched INTEGER CHECK(manual_watched IS NULL OR manual_watched IN (0,1)),
 session_id TEXT,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY(profile_id,timeline_id)
);
CREATE INDEX timeline_viewing_recent ON timeline_viewing(profile_id,updated_at DESC,timeline_id);

CREATE TABLE viewing_sessions (
 id TEXT PRIMARY KEY,
 profile_id TEXT NOT NULL,
 timeline_id TEXT NOT NULL,
 -- The delivery currently presenting the session; replaced by rebinding.
 delivery_id TEXT NOT NULL,
 principal_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision >= 1),
 manual_epoch INTEGER NOT NULL CHECK(manual_epoch >= 0),
 sequence INTEGER NOT NULL CHECK(sequence >= 0),
 position_ms INTEGER NOT NULL CHECK(position_ms >= 0),
 status TEXT NOT NULL CHECK(status IN ('playing','paused','ended','stopped','superseded')),
 duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms >= 0),
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 FOREIGN KEY(profile_id,timeline_id) REFERENCES timeline_viewing(profile_id,timeline_id)
);
CREATE INDEX viewing_sessions_view ON viewing_sessions(profile_id,timeline_id);

-- Acknowledged events. An exact retry replays its stored acknowledgement; a
-- reused sequence or identity with different content is a conflict.
CREATE TABLE viewing_events (
 session_id TEXT NOT NULL REFERENCES viewing_sessions(id),
 sequence INTEGER NOT NULL CHECK(sequence >= 1),
 event_id TEXT NOT NULL,
 delivery_generation INTEGER NOT NULL CHECK(delivery_generation >= 1),
 position_ms INTEGER NOT NULL CHECK(position_ms >= 0),
 status TEXT NOT NULL CHECK(status IN ('playing','paused','ended','stopped')),
 acknowledgement_json TEXT NOT NULL,
 created_at INTEGER NOT NULL,
 PRIMARY KEY(session_id,sequence),
 UNIQUE(session_id,event_id)
);

-- Invalidation hints for profile viewing state (same topic as legacy progress).
CREATE TRIGGER event_timeline_viewing_insert AFTER INSERT ON timeline_viewing BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('viewing',NEW.profile_id); END;
CREATE TRIGGER event_timeline_viewing_update AFTER UPDATE ON timeline_viewing BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('viewing',NEW.profile_id); END;
CREATE TRIGGER event_timeline_viewing_delete AFTER DELETE ON timeline_viewing BEGIN
 INSERT INTO change_events(topic,resource_id) VALUES ('viewing',OLD.profile_id); END;

-- Backfill from legacy work-keyed state only where the timeline is certain:
-- an exact legacy attribution, or a work that has exactly one timeline now.
-- Ambiguous progress stays unattributed. Positions are whole milliseconds.
INSERT INTO timeline_viewing(profile_id,timeline_id,revision,manual_epoch,position_ms,automatic_watched,manual_watched,session_id,updated_at)
SELECT k.profile_id, k.timeline_id, 0, 0,
 CAST(round(max(coalesce(g.position_seconds,0.0),0.0)*1000) AS INTEGER),
 coalesce(v.automatic_watched,0), v.manual_watched, NULL, coalesce(g.updated_at,0)
FROM (
 SELECT p.profile_id, p.item_id,
  coalesce(
   (SELECT a.timeline_id FROM legacy_progress_attribution a
     WHERE a.profile_id=p.profile_id AND a.item_id=p.item_id AND a.outcome='exact'
       AND a.timeline_id IN (SELECT id FROM timelines)),
   (SELECT min(t.id) FROM timelines t JOIN editions e ON e.id=t.edition_id
     WHERE e.item_id=p.item_id HAVING count(*)=1)
  ) AS timeline_id
 FROM (SELECT profile_id,item_id FROM progress UNION SELECT profile_id,item_id FROM viewing_state) p
) k
LEFT JOIN progress g ON g.profile_id=k.profile_id AND g.item_id=k.item_id
LEFT JOIN viewing_state v ON v.profile_id=k.profile_id AND v.item_id=k.item_id
WHERE k.timeline_id IS NOT NULL
 AND k.profile_id IN (SELECT id FROM profiles)
 AND (coalesce(g.position_seconds,0)>0 OR coalesce(v.automatic_watched,0)=1 OR v.manual_watched IS NOT NULL)
-- Two works can never share a timeline, so (profile, timeline) is unique here.
ON CONFLICT(profile_id,timeline_id) DO NOTHING;
