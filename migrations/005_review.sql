-- Assessments whose details Gradebook had to guess (added in Excel, imported, set up "later").
-- 0 = confirmed, 1 = category/weight/due date were guessed, 2 = what it is out of is unknown.
ALTER TABLE assessments ADD COLUMN needs_review INTEGER NOT NULL DEFAULT 0;
