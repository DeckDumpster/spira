-- Adds mending.awaiting_ask for an existing spira_lifecycle database: the open ask a mend waits
-- on, during which no deadline returns the failure. A fresh database gets it from schema.sql.

ALTER TABLE mending ADD COLUMN awaiting_ask VARCHAR(40) NULL;
