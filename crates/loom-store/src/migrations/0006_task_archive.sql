-- Archived tasks (ADR-0013, decision 10): the year of `archive/<yyyy>/<id>/`, NULL for a task
-- in `tasks/`. Every task indexed before this version is in `tasks/`.

ALTER TABLE tasks ADD COLUMN archived INTEGER CHECK (archived IS NULL OR archived BETWEEN 1970 AND 9999);
