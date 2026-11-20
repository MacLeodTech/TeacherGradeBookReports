-- 1 = within each category, newer work counts more than older work (Growing Success: "most consistent, more recent").
ALTER TABLE teacher_settings ADD COLUMN recent_weighting INTEGER NOT NULL DEFAULT 0;
