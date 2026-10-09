-- Adds batch.pass and batch.phase for an existing spira_lifecycle database: the round's pass number
-- and, inside a CI_RUNNING pass, build or suites. A fresh database gets the columns from schema.sql.

ALTER TABLE batch ADD COLUMN pass BIGINT NOT NULL DEFAULT 0;
ALTER TABLE batch ADD COLUMN phase VARCHAR(8) NULL;
