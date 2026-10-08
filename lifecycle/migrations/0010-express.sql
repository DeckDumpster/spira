-- One-time migration for an existing spira_lifecycle database: adds bead.express. Not idempotent;
-- a fresh database gets the column from schema.sql.

ALTER TABLE bead ADD COLUMN express BOOLEAN NOT NULL DEFAULT FALSE;
