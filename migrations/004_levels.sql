-- What each level (R, 1-4) is worth as a percentage, for level assessments. NULL = the default.
ALTER TABLE teacher_settings ADD COLUMN levels_json TEXT;
