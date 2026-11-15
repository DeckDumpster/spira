-- Adds bead.worked_s (seconds of completed WORKING sessions) and bead.sessions (entries into
-- WORKING) for an existing database; 0045 backfills them. A fresh database gets them from schema.sql.

ALTER TABLE bead ADD COLUMN worked_s BIGINT NOT NULL DEFAULT 0;
ALTER TABLE bead ADD COLUMN sessions INT NOT NULL DEFAULT 0;
