-- Adds bead.claimed_from for an existing spira_lifecycle database: the state a WORKING claim was
-- taken from, so a release or dead holder returns REWORK work to REWORK. NULL reads as READY.
-- A fresh database gets the column from schema.sql.

ALTER TABLE bead ADD COLUMN claimed_from VARCHAR(16) NULL;
