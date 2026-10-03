-- One-time migration for an existing spira_lifecycle database: adds bead.since. Not idempotent;
-- a fresh database gets the column from schema.sql.

ALTER TABLE bead ADD COLUMN since BIGINT NULL;
