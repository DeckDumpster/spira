-- Adds batch.progress for an existing spira_lifecycle database: the running pass's live counts
-- (phase, suites done/total, red suites, phase and pass start) as one JSON object. A fresh
-- database gets the column from schema.sql.

ALTER TABLE batch ADD COLUMN progress TEXT NULL;
