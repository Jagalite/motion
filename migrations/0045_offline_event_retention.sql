-- Bounded server retention of consumed offline events
-- (playscale_core::downloads::retain). Each sync may report the device's own
-- compaction point; every event at or below it is deleted and the floor is
-- recorded here, so an unrecorded sequence below it is refused as a stale copy
-- instead of being mistaken for a gap or a new event.
ALTER TABLE offline_sequences ADD COLUMN pruned_through INTEGER NOT NULL DEFAULT 0 CHECK(pruned_through >= 0);
