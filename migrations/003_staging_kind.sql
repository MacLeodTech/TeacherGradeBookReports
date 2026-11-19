-- Staged uploads: "grid" (the import wizard) or "template" (the starter spreadsheet).
ALTER TABLE import_staging ADD COLUMN kind TEXT NOT NULL DEFAULT 'grid';
