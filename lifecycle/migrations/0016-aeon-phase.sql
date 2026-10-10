-- Adds bead.aeon_phase, bead.disposition and bead.disposition_note for an existing spira_lifecycle
-- database: the holder's phase and how its session was cut short, both set only while WORKING.
-- A fresh database gets the columns from schema.sql.

ALTER TABLE bead ADD COLUMN aeon_phase VARCHAR(16) NULL;
ALTER TABLE bead ADD COLUMN disposition VARCHAR(16) NULL;
ALTER TABLE bead ADD COLUMN disposition_note TEXT NULL;
