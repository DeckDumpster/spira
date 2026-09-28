-- One-time migration for an existing spira_lifecycle database: adds the two columns
-- stacked-dependents-2026-09-28 §1 gives the bead row (stack, stack_depth). Not idempotent
-- and not part of schema.sql's own repeated-apply flow — Dolt's ALTER TABLE has no
-- `ADD COLUMN IF NOT EXISTS` (unlike its `CREATE TABLE IF NOT EXISTS`), so re-running this
-- against an already-migrated database fails on "duplicate column" rather than silently
-- doing nothing. A fresh database never needs it: schema.sql's own CREATE TABLE already
-- includes both columns.
--
--   dolt --host H --port P -u root -p PASS --use-db spira_lifecycle sql < migrations/0001-stack.sql

ALTER TABLE bead ADD COLUMN stack JSON NOT NULL DEFAULT (JSON_OBJECT());
ALTER TABLE bead ADD COLUMN stack_depth BIGINT NOT NULL DEFAULT 0;
