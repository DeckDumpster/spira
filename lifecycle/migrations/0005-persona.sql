-- One-time migration for an existing spira_lifecycle database: adds bead.persona. Not idempotent;
-- a fresh database gets the column from schema.sql. Rows claimed before it stay NULL.

ALTER TABLE bead ADD COLUMN persona VARCHAR(64) NULL;
