-- Adds mending.returned for an existing spira_lifecycle database: the JSON array of round
-- members a mender's return sends back. A fresh database gets it from schema.sql.

ALTER TABLE mending ADD COLUMN returned TEXT NULL;
